//! Kusto physical types and documented timestamp precision; logical scalars and digests remain shared.
use chrono::{SecondsFormat, Timelike};
pub use culprit_collector::analytical::projection::{ScalarType, digest};
use culprit_core::{Result, telemetry::DeliveryProblem};
use serde_json::Value;

pub trait KustoType {
    fn kusto_type(self) -> &'static str;
    fn verification_type(self) -> &'static str;
}
impl KustoType for ScalarType {
    fn kusto_type(self) -> &'static str {
        match self {
            Self::String | Self::JsonContainer => "string",
            Self::Bool => "bool",
            Self::Long => "long",
            Self::ExactCounter => "decimal",
            Self::Real => "real",
            Self::Timestamp => "datetime",
        }
    }
    fn verification_type(self) -> &'static str {
        if matches!(self, Self::ExactCounter | Self::JsonContainer) { "string" } else { self.kusto_type() }
    }
}
pub fn source(kind: ScalarType, value: &Value, nullable: bool) -> Result<Value> {
    let value = kind.source(value, nullable)?;
    if kind != ScalarType::Timestamp || value.is_null() {
        return Ok(value);
    }
    let date = chrono::DateTime::parse_from_rfc3339(value.as_str().ok_or_else(invalid)?).map_err(|_| invalid())?;
    let date = date.with_nanosecond(date.nanosecond() / 100 * 100).ok_or_else(invalid)?;
    Ok(Value::String(date.with_timezone(&chrono::Utc).to_rfc3339_opts(SecondsFormat::Nanos, true)))
}
pub fn returned(kind: ScalarType, value: &Value, nullable: bool) -> Result<Value> {
    if nullable && matches!(kind, ScalarType::ExactCounter | ScalarType::JsonContainer) && value.as_str() == Some("") {
        return Ok(Value::Null);
    }
    kind.returned(value, nullable)
}
fn invalid() -> culprit_core::Error {
    DeliveryProblem::Integrity.error()
}
