//! Native layouts and exact facts shared by application projections and background export.
pub(crate) mod activity;
pub(crate) mod catalog;
pub(crate) mod error_log;
pub(crate) mod instance;
pub(crate) mod locks;
pub(crate) mod metadata;
pub(crate) mod operational;
pub(crate) mod replication;
pub(crate) mod runtime_distributions;
pub(crate) mod session_memory;
pub(crate) mod statements;
use crate::{read::QueryRead, rows::Rec};
use culprit_core::{Result, telemetry::CollectionStop};

pub(crate) fn invalid() -> culprit_core::Error {
    CollectionStop::InvalidPage.error()
}
pub(crate) fn text(row: &Rec, field: &str) -> Result<String> {
    row.native_text(field)?.filter(|value| !value.is_empty()).ok_or_else(invalid)
}
pub(crate) fn integer(row: &Rec, field: &str) -> Result<u64> {
    row.unsigned(field)?.ok_or_else(invalid)
}
pub(crate) fn flag(row: &Rec, field: &str) -> Result<bool> {
    match integer(row, field)? {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(invalid()),
    }
}
pub(crate) async fn one<R: QueryRead>(reader: &mut R, sql: &str, parameters: Vec<mysql_async::Value>) -> Result<Rec> {
    let mut rows = reader.read(sql, parameters, 2).await?;
    if rows.len() != 1 {
        return Err(invalid());
    }
    Ok(rows.remove(0))
}
