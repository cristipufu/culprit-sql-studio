//! Native Nullable columns and DateTime64(9) preserve logical nulls and nanoseconds without Kusto presence fields.
use crate::response::Column;
use culprit_collector::analytical::{
    RowProjection,
    projection::{self, ScalarType},
    schema::{LogicalSchema, OutputColumn, encode_row},
};
use culprit_core::{Result, telemetry::DeliveryProblem};
use serde_json::Value;

pub struct Schema {
    logical: LogicalSchema,
    columns: Vec<OutputColumn>,
    digest: String,
}
impl Schema {
    pub fn new(logical: LogicalSchema) -> Result<Self> {
        let bytes =
            serde_json::to_vec(&(logical.digest(), "clickhouse-nullable-datetime64nanos-v1")).map_err(|_| invalid())?;
        Ok(Self { columns: logical.columns(), logical, digest: culprit_core::telemetry::digest(&bytes) })
    }
    pub fn table(&self) -> &str {
        self.logical.table()
    }
    pub fn columns(&self) -> &[OutputColumn] {
        &self.columns
    }
    pub fn column_type(column: &OutputColumn) -> String {
        let kind = match column.kind {
            ScalarType::String | ScalarType::JsonContainer => "String",
            ScalarType::Bool => "Bool",
            ScalarType::Long => "Int64",
            ScalarType::ExactCounter => "Decimal(38, 0)",
            ScalarType::Real => "Float64",
            ScalarType::Timestamp => "DateTime64(9, 'UTC')",
        };
        if column.nullable { format!("Nullable({kind})") } else { kind.into() }
    }
    pub fn verification_columns(&self) -> Vec<Column> {
        self.columns
            .iter()
            .map(|column| {
                let kind = match column.kind {
                    ScalarType::ExactCounter
                    | ScalarType::Timestamp
                    | ScalarType::JsonContainer
                    | ScalarType::String => "String",
                    ScalarType::Bool => "Bool",
                    ScalarType::Long => "Int64",
                    ScalarType::Real => "Float64",
                };
                Column {
                    name: column.name.clone(),
                    kind: if column.nullable { format!("Nullable({kind})") } else { kind.into() },
                }
            })
            .collect()
    }
    pub fn verification_projection(&self) -> String {
        self.columns
            .iter()
            .map(|column| {
                let name = quote(&column.name);
                match column.kind {
                    ScalarType::ExactCounter => format!("toString({name}) AS {name}"),
                    ScalarType::Timestamp => format!("toString(toUnixTimestamp64Nano({name})) AS {name}"),
                    _ => name,
                }
            })
            .collect::<Vec<_>>()
            .join(", ")
    }
    pub fn column_names(&self) -> String {
        self.columns.iter().map(|column| quote(&column.name)).collect::<Vec<_>>().join(", ")
    }
}
impl RowProjection for Schema {
    fn from_logical(schema: LogicalSchema) -> Result<Self> {
        Self::new(schema)
    }
    fn table(&self) -> &str {
        self.table()
    }
    fn digest(&self) -> &str {
        &self.digest
    }
    fn columns(&self) -> &[OutputColumn] {
        self.columns()
    }
    fn project(&self, source: &Value) -> Result<Vec<Value>> {
        let values = self.logical.project(source)?;
        for (column, value) in self.columns.iter().zip(&values) {
            if column.kind == ScalarType::Timestamp && !value.is_null() {
                timestamp_nanos(value)?;
            }
        }
        Ok(values)
    }
    fn encode(&self, values: &[Value]) -> Result<Vec<u8>> {
        encode_row(&self.columns, values)
    }
    fn returned_digest(&self, values: &[Value]) -> Result<[u8; 32]> {
        if values.len() != self.columns.len() {
            return Err(invalid());
        }
        let normalized = self
            .columns
            .iter()
            .zip(values)
            .map(|(column, value)| {
                if column.kind == ScalarType::Timestamp && !value.is_null() {
                    let nanos = value.as_str().and_then(|s| s.parse::<i64>().ok()).ok_or_else(invalid)?;
                    let date = chrono::DateTime::from_timestamp_nanos(nanos);
                    return Ok(Value::String(date.to_rfc3339_opts(chrono::SecondsFormat::Nanos, true)));
                }
                column.kind.returned(value, column.nullable)
            })
            .collect::<Result<Vec<_>>>()?;
        projection::digest(&normalized)
    }
}
/// Reject DateTime64(9) overflow before dispatch; never silently round or wrap a native timestamp.
fn timestamp_nanos(value: &Value) -> Result<i64> {
    chrono::DateTime::parse_from_rfc3339(value.as_str().ok_or_else(invalid)?)
        .map_err(|_| invalid())?
        .timestamp_nanos_opt()
        .ok_or_else(invalid)
}
pub fn quote(name: &str) -> String {
    format!("`{}`", name.replace('\\', "\\\\").replace('`', "\\`"))
}
fn invalid() -> culprit_core::Error {
    DeliveryProblem::Integrity.error()
}

#[cfg(test)]
mod tests;
