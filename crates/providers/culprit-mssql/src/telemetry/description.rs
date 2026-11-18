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
    pub acquisition: super::history_acquisition::HistoryAcquisition,
    pub layout: crate::query_store::layout::NativeLayout,
    pub plan_upper_bound: i64,
    pub replica_group_ids: Vec<String>,
}
impl Description {
    pub(super) fn range_population(&self) -> super::cursor::PopulationRange {
        if !self.layout.partitioned {
            super::cursor::PopulationRange::Unpartitioned
        } else if self.replica_group_ids.is_empty() {
            super::cursor::PopulationRange::AllGroups
        } else {
            super::cursor::PopulationRange::Catalog
        }
    }
    pub fn waits_supported(&self) -> bool {
        self.value
            .query_history()
            .and_then(|history| history.waits.as_ref())
            .is_some_and(|waits| waits.capture != WaitCaptureState::Unavailable)
    }
}

pub(super) struct DatabaseProfile {
    pub configured_log_rates: Result<bool>,
}

pub(super) async fn ensure_supported<R: NativeRead>(session: &mut R) -> Result<()> {
    supported_profile(session, crate::database_identity::log_rates::Detail::Identity).await.map(|_| ())
}

pub(super) async fn supported_profile<R: NativeRead>(
    session: &mut R,
    detail: crate::database_identity::log_rates::Detail,
) -> Result<DatabaseProfile> {
    let query = match detail {
        crate::database_identity::log_rates::Detail::Identity => sql::EDITION.to_owned(),
        crate::database_identity::log_rates::Detail::ConfiguredLogRates => format!(
            "SELECT /* culprit */ native.edition,{} AS configured_log_rate_capability FROM ({}) native",
            crate::database_identity::log_rates::CAPABILITY,
            sql::EDITION
        ),
    };
    let rows = session.query(&query, vec![], 1).await.map_err(ReadError::into_error)?;
    let Some(row) = rows.first() else {
        return Err(Error::new(ErrorCode::Unsupported, t!("collection.azure_sql_only")));
    };
    if integer(row, "edition")? != 5 {
        return Err(Error::new(ErrorCode::Unsupported, t!("collection.azure_sql_only")));
    }
    let configured_log_rates = match detail {
        crate::database_identity::log_rates::Detail::Identity => Ok(false),
        crate::database_identity::log_rates::Detail::ConfiguredLogRates => {
            crate::database_identity::log_rates::capability(row)
        }
    };
    Ok(DatabaseProfile { configured_log_rates })
}

pub(super) async fn read<R: NativeRead>(
    session: &mut R,
    request: &CollectionRequest,
    acquisition: super::history_acquisition::HistoryAcquisition,
) -> Result<Description> {
    let settings = settings(session).await?;
    if settings.layout.edition != acquisition.edition() {
        return Err(culprit_core::telemetry::CollectionStop::SourceChanged.error());
    }
    let target = acquisition.target();
    let interval = crate::query_store::intervals::exact(session, request).await?;
    let replicas = if settings.layout.partitioned {
        crate::query_store::replicas::read(session).await?
    } else {
        if settings.layout.mode == crate::query_store::mode::ReadMode::ReadCaptureSecondary {
            return Err(Error::new(ErrorCode::Unsupported, t!("provider.mssql.query_store.population_unavailable")));
        }
        None
    };
    let replica_group_ids = replicas.as_ref().map_or_else(Vec::new, |catalog| catalog.ids());
    let population_catalog = match replicas {
        Some(replicas) => Some(replicas.population_catalog()?.native_facts()),
        None if !settings.layout.partitioned => {
            Some(crate::query_store::replicas::unpartitioned_catalog().native_facts())
        }
        None => None,
    };
    let population_binding = match &population_catalog {
        Some(catalog) if !catalog.partitioned => culprit_core::telemetry::QueryHistoryPopulationBinding::Unpartitioned,
        Some(catalog) => QueryHistoryPopulationBinding::Dimensions {
            bindings: catalog
                .populations
                .iter()
                .map(|population| QueryHistoryPopulationBindingEntry {
                    population: population.identity.clone(),
                    qualifiers: [("replica_group_id".into(), population.identity.id.clone())].into(),
                })
                .collect(),
        },
        None => QueryHistoryPopulationBinding::Unknown,
    };
    let dimensions = if replica_group_ids.is_empty() {
        Default::default()
    } else {
        [("replica_group_id".into(), replica_group_ids.clone())].into()
    };
    let plan_upper_bound = plan_upper_bound(session).await?;
    Ok(Description {
        value: CollectionDescription {
            target,
            partition: culprit_core::telemetry::SourcePartition::Target,
            source: culprit_core::source_id::QUERY_STORE.into(),
            observation: culprit_core::telemetry::SourceObservation::QueryHistory(
                culprit_core::telemetry::QueryHistoryDescription {
                    interval,
                    acquisition_owner: Some(Box::new(acquisition.owner()?)),
                    population_catalog,
                    population_binding,
                    dimensions,
                    native_properties: [
                        ("capture_mode".into(), settings.capture_mode),
                        ("runtime_replica_dimension".into(), settings.layout.partitioned.to_string()),
                        ("tempdb_metric_available".into(), settings.layout.tempdb.to_string()),
                        ("log_bytes_metric_available".into(), settings.layout.log_bytes.to_string()),
                        ("waits_view_available".into(), settings.layout.waits.to_string()),
                        ("actual_state".into(), settings.actual_state),
                        ("local_capture_permitted".into(), settings.layout.mode.accepts_local_capture().to_string()),
                        ("variant_families".into(), settings.layout.variants.native_property().into()),
                        (
                            culprit_core::telemetry::PageServerReadAvailability::PROPERTY.into(),
                            settings.layout.page_server_reads.native_property().into(),
                        ),
                    ]
                    .into(),
                    waits: Some(waits::semantics(settings.wait_capture)),
                },
            ),
            continuity: SourceContinuity::Unverified,
        },
        acquisition,
        layout: settings.layout,
        plan_upper_bound,
        replica_group_ids,
    })
}

pub(super) struct Settings {
    pub(super) layout: crate::query_store::layout::NativeLayout,
    capture_mode: String,
    actual_state: String,
    wait_capture: WaitCaptureState,
}

pub(super) async fn settings<R: NativeRead>(session: &mut R) -> Result<Settings> {
    let rows = session.query(sql::OPTIONS, vec![], 1).await.map_err(ReadError::into_error)?;
    let [row] = rows.as_slice() else { return Err(malformed()) };
    if crate::read::optional_boolean(row, "metadata_visibility")? != Some(true) {
        return Err(crate::catalog_visibility::unavailable());
    }
    let layout = crate::query_store::layout::NativeLayout::decode(row)?;
    let actual_state = text(row, "actual_state_desc")?;
    Ok(Settings {
        layout,
        actual_state,
        capture_mode: text(row, "query_capture_mode_desc")?,
        wait_capture: if layout.waits { waits::capture_state(row)? } else { WaitCaptureState::Unavailable },
    })
}

pub(super) async fn identity<R: NativeRead>(
    session: &mut R,
    host: &str,
    port: u16,
) -> Result<(DatabaseIdentity, String)> {
    let context = identity_detail(session, host, port, crate::database_identity::log_rates::Detail::Identity).await?;
    Ok((context.database, context.physical_incarnation))
}

/// Query Store history is a logical-database authority replicated to readable members. Reading it does not
/// authorize replica-local operational sources or establish that this member captured the retained executions.
pub(crate) async fn query_history_identity<R: NativeRead>(
    session: &mut R,
    host: &str,
    port: u16,
) -> Result<(DatabaseIdentity, String)> {
    let context = database_context(session, host, port).await?;
    Ok((context.database, context.physical_incarnation))
}

pub(super) struct DatabaseContext {
    pub database: DatabaseIdentity,
    pub physical_incarnation: String,
    pub configured_log_rates: Result<Option<culprit_core::telemetry::ConfiguredLogRateLimits>>,
}

pub(super) async fn identity_detail<R: NativeRead>(
    session: &mut R,
    host: &str,
    port: u16,
    detail: crate::database_identity::log_rates::Detail,
) -> Result<DatabaseContext> {
    let read = crate::database_identity::read_azure_detail(session, detail).await?;
    crate::read::require_primary(read.context.writable)?;
    resolved_database_context(read, host, port)
}

async fn database_context<R: NativeRead>(session: &mut R, host: &str, port: u16) -> Result<DatabaseContext> {
    let context = crate::database_identity::read_azure(session).await?;
    resolved_database_context(
        crate::database_identity::GovernanceRead { context, configured_log_rates: Ok(None) },
        host,
        port,
    )
}

fn resolved_database_context(
    read: crate::database_identity::GovernanceRead,
    host: &str,
    port: u16,
) -> Result<DatabaseContext> {
    let context = read.context;
    Ok(DatabaseContext {
        database: context.identity(host, port),
        physical_incarnation: context.physical_incarnation().ok_or_else(malformed)?.to_owned(),
        configured_log_rates: read.configured_log_rates,
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
        crate::resources::test_support::replace(
            &super::super::history_tests::options(5, true),
            &[("actual_state_desc", Val::Text(state.into()))],
        )
    }

    #[tokio::test]
    async fn native_capture_mode_is_qualified_independently_from_historical_readability() {
        for (state, capture) in [("READ_CAPTURE_SECONDARY", true), ("READ_WRITE", true), ("READ_ONLY", false)] {
            let mut reader = FakeRead::new(vec![Ok(vec![options(state)])]);
            let settings = settings(&mut reader).await.unwrap();
            assert_eq!(settings.actual_state, state);
            assert_eq!(settings.layout.mode.accepts_local_capture(), capture);
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
