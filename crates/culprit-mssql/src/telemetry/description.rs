use crate::query_store::{
    catalog_sql as sql,
    decode::{integer, malformed, text},
};
use crate::read::{NativeRead, ReadError};
use culprit_core::{
    DatabaseIdentity, Engine, Error, ErrorCode, Result, t,
    telemetry::{CollectionDescription, CollectionRequest, SourceContinuity},
};

pub(super) struct Description {
    pub value: CollectionDescription,
    pub physical_incarnation: String,
    pub plan_upper_bound: i64,
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
    let (capture_mode, waits_capture_enabled) = settings(session).await?;
    let (database, physical_incarnation) = identity(session, host, port).await?;
    let interval = crate::query_store::intervals::exact(session, request).await?;
    let replica_group_ids = replicas(session).await?;
    let plan_upper_bound = plan_upper_bound(session).await?;
    Ok(Description {
        value: CollectionDescription {
            database,
            source: culprit_core::source_id::QUERY_STORE.into(),
            observation: culprit_core::telemetry::SourceObservation::QueryHistory(
                culprit_core::telemetry::QueryHistoryDescription {
                    interval,
                    replica_group_ids,
                    capture_mode,
                    waits_capture_enabled,
                },
            ),
            continuity: SourceContinuity::Unverified,
        },
        physical_incarnation,
        plan_upper_bound,
    })
}

pub(super) async fn settings<R: NativeRead>(session: &mut R) -> Result<(String, bool)> {
    let rows = session.query(sql::OPTIONS, vec![], 1).await.map_err(ReadError::into_error)?;
    let row = rows.first().ok_or_else(malformed)?;
    if !matches!(text(row, "actual_state_desc")?.as_str(), "READ_WRITE" | "READ_ONLY") {
        return Err(Error::new(ErrorCode::Unsupported, t!("collection.query_store_unavailable")));
    }
    Ok((text(row, "query_capture_mode_desc")?, text(row, "wait_stats_capture_mode_desc")? == "ON"))
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
    require_primary(crate::read::database_writable(row)?)?;
    let mut database = DatabaseIdentity::server(Engine::Mssql, &text(row, "database_name")?, host, port, None);
    database.incarnation = Some(text(row, "incarnation")?);
    Ok((database, text(row, "physical_incarnation")?))
}
pub(crate) fn require_primary(writable: Option<bool>) -> Result<()> {
    if writable == Some(true) {
        Ok(())
    } else {
        Err(Error::new(ErrorCode::Unsupported, t!("collection.azure_sql_primary_only")))
    }
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
