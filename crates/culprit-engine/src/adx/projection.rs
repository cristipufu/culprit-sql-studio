//! Typed projection checks compare every returned value with its expected projection, not an opaque source hash.
//! Digests are computed in Rust on both sides; they do not depend on KQL's JSON or floating-point formatting.
use chrono::{Datelike, SecondsFormat, Timelike};
use culprit_core::{Result, telemetry::DeliveryProblem};
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};

mod canonical;
#[cfg(test)]
mod tests;

const MAX_COLUMN_BYTES: usize = 256 * 1024;
pub const PROJECTION_NORMALIZATION_VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum ScalarType {
    String,
    Bool,
    Long,
    ExactCounter,
    Real,
    Timestamp,
    JsonContainer,
}
impl ScalarType {
    pub fn kusto_type(self) -> &'static str {
        match self {
            Self::String => "string",
            Self::Bool => "bool",
            Self::Long => "long",
            Self::ExactCounter => "decimal",
            Self::Real => "real",
            Self::Timestamp => "datetime",
            Self::JsonContainer => "dynamic",
        }
    }
    /// Decimal counters are selected with tostring(Column) during verification, preserving all uint64 digits.
    pub fn verification_type(self) -> &'static str {
        if matches!(self, Self::ExactCounter | Self::JsonContainer) { "string" } else { self.kusto_type() }
    }
    pub fn source(self, value: &Value, nullable: bool) -> Result<Value> {
        self.normalize(value, nullable, false)
    }
    pub fn returned(self, value: &Value, nullable: bool) -> Result<Value> {
        self.normalize(value, nullable, true)
    }
    fn normalize(self, value: &Value, nullable: bool, returned: bool) -> Result<Value> {
        if value.is_null() {
            return if nullable && (self != Self::String || !returned) { Ok(Value::Null) } else { Err(invalid()) };
        }
        match self {
            Self::String => value
                .as_str()
                .filter(|text| text.len() <= MAX_COLUMN_BYTES)
                .map(|text| Value::String(text.into()))
                .ok_or_else(invalid),
            Self::Bool => value.as_bool().map(Value::Bool).ok_or_else(invalid),
            Self::Long => {
                let integer = value.as_i64().or_else(|| value.as_str()?.parse::<i64>().ok()).ok_or_else(invalid)?;
                Ok(Value::from(integer))
            }
            Self::ExactCounter => {
                let text = value.as_str().ok_or_else(invalid)?;
                if returned && nullable && text.is_empty() {
                    return Ok(Value::Null);
                }
                canonical::counter(text).map(Value::String)
            }
            Self::Real => {
                let real = value.as_f64().filter(|value| value.is_finite()).ok_or_else(invalid)?;
                Ok(Value::from(if real == 0.0 { 0.0 } else { real }))
            }
            Self::Timestamp => timestamp(value),
            Self::JsonContainer => {
                let parsed;
                let value = if returned {
                    let text = value.as_str().ok_or_else(invalid)?;
                    if text.is_empty() && nullable {
                        return Ok(Value::Null);
                    }
                    if text.len() > MAX_COLUMN_BYTES {
                        return Err(DeliveryProblem::Capacity.error());
                    }
                    parsed = serde_json::from_str::<Value>(text).map_err(|_| invalid())?;
                    &parsed
                } else {
                    value
                };
                if !(value.is_object() || value.is_array()) {
                    return Err(invalid());
                }
                if serde_json::to_vec(value).map_err(|_| invalid())?.len() > MAX_COLUMN_BYTES {
                    return Err(DeliveryProblem::Capacity.error());
                }
                Ok(value.clone())
            }
        }
    }
}
fn timestamp(value: &Value) -> Result<Value> {
    let value = value.as_str().ok_or_else(invalid)?;
    let date = chrono::DateTime::parse_from_rfc3339(value).map_err(|_| invalid())?.with_timezone(&chrono::Utc);
    if !(1..=9999).contains(&date.year()) || date.nanosecond() >= 1_000_000_000 {
        return Err(invalid());
    }
    // Kusto datetime has 100 ns resolution. Preserve original native timestamps in recovery storage.
    let date = date.with_nanosecond(date.nanosecond() / 100 * 100).ok_or_else(invalid)?;
    Ok(Value::String(date.to_rfc3339_opts(SecondsFormat::Nanos, true)))
}
/// The schema fixes column order and types before this function is called. Include that schema's digest separately
/// in the immutable projection-index key; a schema change cannot adopt an earlier index.
pub fn digest(values: &[Value]) -> Result<[u8; 32]> {
    let mut hash = Sha256::new();
    hash.update(PROJECTION_NORMALIZATION_VERSION.to_be_bytes());
    canonical::append_array(values, &mut hash)?;
    Ok(hash.finalize().into())
}
fn invalid() -> culprit_core::Error {
    DeliveryProblem::Integrity.error()
}
