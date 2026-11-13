//! Native statistics readers shared by interactive projection and background capture.
pub(crate) mod activity;
pub(crate) mod catalog;
pub(crate) mod cleanup;
pub(crate) mod indexes;
pub(crate) mod instance;
pub(crate) mod memory;
pub(crate) mod metadata;
pub(crate) mod operational;
pub(crate) mod relations;
pub(crate) mod replication;
pub(crate) mod sequences;
pub(crate) mod settings;
pub(crate) mod statements;
use crate::{read::QueryRead, rows::Row};
use culprit_core::{Result, telemetry::CollectionStop};

pub(crate) fn invalid() -> culprit_core::Error {
    CollectionStop::InvalidPage.error()
}
pub(crate) fn text(row: &Row, field: &str) -> Result<String> {
    row.text(field).filter(|value| !value.is_empty()).ok_or_else(invalid)
}
pub(crate) fn integer(row: &Row, field: &str) -> Result<u64> {
    text(row, field)?.parse::<u64>().map_err(|_| invalid())
}
pub(crate) fn flag(row: &Row, field: &str) -> Result<bool> {
    match row.value(field) {
        Some(Some("t" | "true" | "on" | "1")) => Ok(true),
        Some(Some("f" | "false" | "off" | "0")) => Ok(false),
        _ => Err(invalid()),
    }
}
pub(crate) async fn one<R: QueryRead>(reader: &mut R, sql: &str) -> Result<Row> {
    let mut rows = reader.query(sql, 2).await?;
    if rows.len() != 1 {
        return Err(invalid());
    }
    Ok(rows.remove(0))
}
pub(crate) fn iso(expression: &str) -> String {
    format!("pg_catalog.to_char(({expression}) AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"')")
}
