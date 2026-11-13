//! A complete typed JSONCompact document is mandatory; HTTP 200 alone cannot prove query success.
use culprit_core::{Result, telemetry::DeliveryProblem};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub use culprit_collector::analytical::read_store::MAX_READ_RESPONSE_BYTES as MAX_RESPONSE_BYTES;
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
    pub response_bytes: usize,
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
pub fn parse(bytes: &[u8], columns: &[Column], row_limit: usize, byte_limit: usize) -> Result<QueryTable> {
    if byte_limit == 0
        || byte_limit > MAX_RESPONSE_BYTES
        || bytes.len() > byte_limit
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
    Ok(QueryTable { columns: response.meta, rows: response.data, response_bytes: bytes.len() })
}
fn invalid() -> culprit_core::Error {
    DeliveryProblem::Integrity.error()
}
#[cfg(test)]
mod tests;
