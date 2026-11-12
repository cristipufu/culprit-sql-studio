//! A complete typed JSONCompact document is mandatory; HTTP 200 alone cannot prove query success.
use culprit_core::{Result, telemetry::DeliveryProblem};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const MAX_RESPONSE_BYTES: usize = 16 * 1024 * 1024;
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Column {
    pub name: String,
    #[serde(rename = "type")]
    pub kind: String,
}
pub struct QueryTable {
    pub columns: Vec<Column>,
    pub rows: Vec<Vec<Value>>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Response {
    meta: Vec<Column>,
    data: Vec<Vec<Value>>,
    rows: usize,
    #[serde(default)]
    rows_before_limit_at_least: Option<u64>,
    #[serde(default)]
    statistics: Option<Value>,
}
pub fn parse(bytes: &[u8], columns: &[Column], row_limit: usize) -> Result<QueryTable> {
    if bytes.len() > MAX_RESPONSE_BYTES
        || columns.is_empty()
        || columns.len() > 128
        || row_limit == 0
        || row_limit > 10_001
    {
        return Err(DeliveryProblem::Capacity.error());
    }
    let response: Response = serde_json::from_slice(bytes).map_err(|_| invalid())?;
    let _ = (response.rows_before_limit_at_least, response.statistics);
    if response.meta != columns
        || response.rows != response.data.len()
        || response.data.len() > row_limit
        || response.data.iter().any(|row| row.len() != columns.len())
    {
        return Err(invalid());
    }
    Ok(QueryTable { columns: response.meta, rows: response.data })
}
fn invalid() -> culprit_core::Error {
    DeliveryProblem::Integrity.error()
}
#[cfg(test)]
mod tests;
