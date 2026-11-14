use crate::query_store::{
    catalog_sql as sql,
    decode::{integer, malformed, text},
    waits,
};
use crate::read::{NativeRead, ReadError};
use culprit_core::{
    DatabaseIdentity, Engine, Error, ErrorCode, Result, t,
    telemetry::{CollectionDescription, CollectionRequest, SourceContinuity, WaitCaptureState},
};

pub(super) struct Description {
    pub value: CollectionDescription,
    pub physical_incarnation: String,
    pub plan_upper_bound: i64,
    pub replica_group_ids: Vec<String>,
    pub variant_families: crate::query_store::variants_sql::Availability,
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
    let (database, physical_incarnation) = identity(session, host, port).await?;
    let interval = crate::query_store::intervals::exact(session, request).await?;
    let replica_group_ids = replicas(session).await?;
    let plan_upper_bound = plan_upper_bound(session).await?;
    Ok(Description {
        value: CollectionDescription {
            target: database.into(),
            source: culprit_core::source_id::QUERY_STORE.into(),
            observation: culprit_core::telemetry::SourceObservation::QueryHistory(
                culprit_core::telemetry::QueryHistoryDescription {
                    interval,
                    dimensions: [("replica_group_id".into(), replica_group_ids.clone())].into(),
                    native_properties: [
                        ("capture_mode".into(), settings.capture_mode),
                        ("variant_families".into(), settings.variant_families.native_property().into()),
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
    })
}

pub(super) struct Settings {
    capture_mode: String,
    wait_capture: WaitCaptureState,
    variant_families: crate::query_store::variants_sql::Availability,
}

pub(super) async fn settings<R: NativeRead>(session: &mut R) -> Result<Settings> {
    let rows = session.query(sql::OPTIONS, vec![], 1).await.map_err(ReadError::into_error)?;
    let row = rows.first().ok_or_else(malformed)?;
    if !matches!(text(row, "actual_state_desc")?.as_str(), "READ_WRITE" | "READ_ONLY") {
        return Err(Error::new(ErrorCode::Unsupported, t!("collection.query_store_unavailable")));
    }
    let variant_families = crate::query_store::variants_sql::Availability::decode(row, "variant_families")?;
    Ok(Settings {
        capture_mode: text(row, "query_capture_mode_desc")?,
        wait_capture: waits::capture_state(row)?,
        variant_families,
    })
}

pub(super) async fn identity<R: NativeRead>(
    session: &mut R,
    host: &str,
    port: u16,
) -> Result<(DatabaseIdentity, String)> {
    let rows = session.query(sql::IDENTITY, vec![], 2).await.map_err(ReadError::into_error)?;
    if rows.len() != 1 {
        return Err(malformed());
    }
    let row = &rows[0];
    crate::read::require_primary(crate::read::database_writable(row)?)?;
    let mut database = DatabaseIdentity::server(Engine::Mssql, &text(row, "database_name")?, host, port, None);
    database.incarnation = Some(text(row, "incarnation")?);
    Ok((database, text(row, "physical_incarnation")?))
}

async fn replicas<R: NativeRead>(session: &mut R) -> Result<Vec<String>> {
    let rows = session.query(sql::REPLICAS, vec![], 101).await.map_err(ReadError::into_error)?;
    if rows.is_empty() || rows.len() > 100 {
        return Err(malformed());
    }
    let mut ids =
        rows.iter().map(|row| integer(row, "replica_group_id").map(|id| id.to_string())).collect::<Result<Vec<_>>>()?;
    ids.sort();
    ids.dedup();
    Ok(ids)
}

async fn plan_upper_bound<R: NativeRead>(session: &mut R) -> Result<i64> {
    let rows = session.query(sql::MAX_PLAN, vec![], 1).await.map_err(ReadError::into_error)?;
    rows.first().map(|row| integer(row, "plan_id")).transpose()?.unwrap_or(0).checked_add(1).ok_or_else(malformed)
}
