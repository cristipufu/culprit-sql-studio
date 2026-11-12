//! YSQL/DocDB source facts. Wire ownership and SQL catalog grammar are shared; storage and sampling semantics are native.
pub(crate) mod metadata;
pub(crate) mod operational;
pub(crate) mod plans;
pub(crate) mod schema;
pub(crate) mod statements;
use culprit_core::{Result, telemetry::*};
use culprit_postgres::wire::{QueryRead, Row};
pub(crate) fn invalid() -> culprit_core::Error {
    CollectionStop::InvalidPage.error()
}
pub(crate) fn text(row: &Row, name: &str) -> Result<String> {
    row.text(name).filter(|value| !value.is_empty()).ok_or_else(invalid)
}
pub(crate) fn integer(row: &Row, name: &str) -> Result<u64> {
    text(row, name)?.parse().map_err(|_| invalid())
}
pub(crate) fn flag(row: &Row, name: &str) -> Result<bool> {
    match row.text(name).as_deref() {
        Some("t" | "true" | "on") => Ok(true),
        Some("f" | "false" | "off") => Ok(false),
        _ => Err(invalid()),
    }
}
pub(crate) fn number(row: &Row, name: &str) -> Result<f64> {
    text(row, name)?.parse::<f64>().ok().filter(|value| value.is_finite() && *value >= 0.0).ok_or_else(invalid)
}
pub(crate) async fn one(reader: &mut impl QueryRead, sql: &str) -> Result<Row> {
    let mut rows = reader.query(sql, 2).await?;
    if rows.len() != 1 {
        return Err(invalid());
    }
    Ok(rows.remove(0))
}
