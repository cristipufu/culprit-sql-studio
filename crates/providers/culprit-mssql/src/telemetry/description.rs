use crate::query_store::{
    catalog_sql as sql,
    decode::{integer, malformed, text},
    waits,
};
use crate::read::{NativeRead, ReadError};
use culprit_core::{
    DatabaseIdentity, Error, ErrorCode, Result, t,
    telemetry::{
        CollectionDescription, CollectionRequest, QueryHistoryPopulationBinding, QueryHistoryPopulationBindingEntry,
        SourceContinuity, WaitCaptureState,
    },
};

pub(super) struct Description {
    pub value: CollectionDescription,
    pub physical_incarnation: String,
    pub plan_upper_bound: i64,
    pub replica_group_ids: Vec<String>,
    pub variant_families: crate::query_store::variants_sql::Availability,
    pub page_server_reads: crate::query_store::page_server::Availability,
}
impl Description {
    pub fn waits_supported(&self) -> bool {
        self.value
            .query_history()
            .and_then(|history| history.waits.as_ref())
            .is_some_and(|waits| waits.capture != WaitCaptureState::Unavailable)
    }
}

pub(super) async fn ensure_supported<R: NativeRead>(session: &mut R) -> Result<()> {
    let rows = session.query(sql::EDITION, vec![], 1).await.map_err(ReadError::into_error)?;
    if rows.first().map(|row| integer(row, "edition")).transpose()? == Some(5) {
        return Ok(());
    }
    Err(Error::new(ErrorCode::Unsupported, t!("collection.azure_sql_only")))
}

pub(super) async fn read<R: NativeRead>(
    session: &mut R,
    request: &CollectionRequest,
    host: &str,
    port: u16,
) -> Result<Description> {
    let settings = settings(session).await?;
    let (database, physical_incarnation) = query_history_identity(session, host, port).await?;
    let interval = crate::query_store::intervals::exact(session, request).await?;
    let replicas = crate::query_store::replicas::read(session).await?;
    let replica_group_ids = replicas.ids();
    let population_catalog = replicas.population_catalog()?.native_facts();
    let population_binding = QueryHistoryPopulationBinding::Dimensions {
        bindings: population_catalog
            .populations
            .iter()
            .map(|population| QueryHistoryPopulationBindingEntry {
                population: population.identity.clone(),
                qualifiers: [("replica_group_id".into(), population.identity.id.clone())].into(),
            })
            .collect(),
    };
    let plan_upper_bound = plan_upper_bound(session).await?;
    Ok(Description {
        value: CollectionDescription {
            target: database.into(),
            partition: culprit_core::telemetry::SourcePartition::Target,
            source: culprit_core::source_id::QUERY_STORE.into(),
            observation: culprit_core::telemetry::SourceObservation::QueryHistory(
                culprit_core::telemetry::QueryHistoryDescription {
                    interval,
                    population_catalog: Some(population_catalog),
                    population_binding,
                    dimensions: [("replica_group_id".into(), replica_group_ids.clone())].into(),
                    native_properties: [
                        ("capture_mode".into(), settings.capture_mode),
                        ("actual_state".into(), settings.actual_state),
                        ("local_capture_permitted".into(), settings.mode.accepts_local_capture().to_string()),
                        ("variant_families".into(), settings.variant_families.native_property().into()),
                        (
                            culprit_core::telemetry::PageServerReadAvailability::PROPERTY.into(),
                            settings.page_server_reads.native_property().into(),
                        ),
                    ]
                    .into(),
                    waits: Some(waits::semantics(settings.wait_capture)),
                },
            ),
            continuity: SourceContinuity::Unverified,
        },
        physical_incarnation,
        plan_upper_bound,
        replica_group_ids,
        variant_families: settings.variant_families,
        page_server_reads: settings.page_server_reads,
    })
}

pub(super) struct Settings {
    capture_mode: String,
    actual_state: String,
    mode: crate::query_store::mode::ReadMode,
    wait_capture: WaitCaptureState,
    variant_families: crate::query_store::variants_sql::Availability,
    page_server_reads: crate::query_store::page_server::Availability,
}

pub(super) async fn settings<R: NativeRead>(session: &mut R) -> Result<Settings> {
    let rows = session.query(sql::OPTIONS, vec![], 1).await.map_err(ReadError::into_error)?;
    let row = rows.first().ok_or_else(malformed)?;
    let actual_state = text(row, "actual_state_desc")?;
    let mode = crate::query_store::mode::ReadMode::from_native(&actual_state)
        .ok_or_else(|| Error::new(ErrorCode::Unsupported, t!("collection.query_store_unavailable")))?;
    let variant_families = crate::query_store::variants_sql::Availability::decode(row, "variant_families")?;
    Ok(Settings {
        actual_state,
        mode,
        capture_mode: text(row, "query_capture_mode_desc")?,
        wait_capture: waits::capture_state(row)?,
        variant_families,
        page_server_reads: crate::query_store::page_server::decode_availability(row)?,
    })
}

pub(super) async fn identity<R: NativeRead>(
    session: &mut R,
    host: &str,
    port: u16,
) -> Result<(DatabaseIdentity, String)> {
    let context = database_context(session, host, port).await?;
    crate::read::require_primary(context.writable)?;
    Ok((context.database, context.physical_incarnation))
}

/// Query Store history is a logical-database authority replicated to readable members. Reading it does not
/// authorize replica-local operational sources or establish that this member captured the retained executions.
pub(super) async fn query_history_identity<R: NativeRead>(
    session: &mut R,
    host: &str,
    port: u16,
) -> Result<(DatabaseIdentity, String)> {
    let context = database_context(session, host, port).await?;
    Ok((context.database, context.physical_incarnation))
}

struct DatabaseContext {
    database: DatabaseIdentity,
    physical_incarnation: String,
    writable: Option<bool>,
}

async fn database_context<R: NativeRead>(session: &mut R, host: &str, port: u16) -> Result<DatabaseContext> {
    let context = crate::database_identity::read_azure(session).await?;
    Ok(DatabaseContext {
        database: context.identity(host, port),
        physical_incarnation: context.physical_incarnation().ok_or_else(malformed)?.to_owned(),
        writable: context.writable,
    })
}

async fn plan_upper_bound<R: NativeRead>(session: &mut R) -> Result<i64> {
    let rows = session.query(sql::MAX_PLAN, vec![], 1).await.map_err(ReadError::into_error)?;
    rows.first().map(|row| integer(row, "plan_id")).transpose()?.unwrap_or(0).checked_add(1).ok_or_else(malformed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        resources::test_support::FakeRead,
        value::{Rec, Val},
    };

    fn options(state: &str) -> Rec {
        Rec::of(&[
            ("actual_state_desc", Val::Text(state.into())),
            ("query_capture_mode_desc", Val::Text("AUTO".into())),
            ("wait_stats_capture_mode_desc", Val::Text("ON".into())),
            ("variant_families", Val::Int(1)),
            ("page_server_read_availability", Val::Int(0)),
        ])
    }

    #[tokio::test]
    async fn native_capture_mode_is_qualified_independently_from_historical_readability() {
        for (state, capture) in [("READ_CAPTURE_SECONDARY", true), ("READ_WRITE", true), ("READ_ONLY", false)] {
            let mut reader = FakeRead::new(vec![Ok(vec![options(state)])]);
            let settings = settings(&mut reader).await.unwrap();
            assert_eq!(settings.actual_state, state);
            assert_eq!(settings.mode.accepts_local_capture(), capture);
            assert_eq!(settings.wait_capture, WaitCaptureState::Enabled);
            assert_eq!(reader.calls.len(), 1);
        }
    }

    #[tokio::test]
    async fn unknown_native_mode_is_unavailable_without_followup_or_configuration_changes() {
        let mut reader = FakeRead::new(vec![Ok(vec![options("FUTURE_MODE")])]);
        let error = settings(&mut reader).await.err().unwrap();
        assert_eq!(error.code, ErrorCode::Unsupported);
        assert_eq!(reader.calls.len(), 1);
        assert!(!reader.calls[0].sql.contains("ALTER"));
    }

    #[tokio::test]
    async fn logical_history_on_a_readable_replica_does_not_authorize_local_primary_only_sources() {
        let row = Rec::of(&[
            ("database_name", Val::Text("fixture".into())),
            ("incarnation", Val::Text("logical".into())),
            ("physical_incarnation", Val::Text("physical".into())),
            ("updateability", Val::Text("READ_ONLY".into())),
        ]);
        let mut history = FakeRead::new(vec![Ok(vec![row.clone()])]);
        let (database, physical) = query_history_identity(&mut history, "fixture.invalid", 1433).await.unwrap();
        assert_eq!(database.incarnation.as_deref(), Some("logical"));
        assert_eq!(physical, "physical");
        assert_eq!(history.calls.len(), 1);

        let mut operational = FakeRead::new(vec![Ok(vec![row])]);
        let error = identity(&mut operational, "fixture.invalid", 1433).await.err().unwrap();
        assert_eq!(error.code, ErrorCode::Unsupported);
        assert_eq!(operational.calls.len(), 1);
        assert_eq!(history.calls[0].sql, operational.calls[0].sql, "one native identity projection/decoder");
    }
}
