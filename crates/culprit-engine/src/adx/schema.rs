//! One declaration drives typed projection, deployment mappings and exact verification columns.
use super::{
    projection::{self, ScalarType},
    response::Column,
};
use culprit_core::{Result, telemetry::DeliveryProblem};
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::BTreeSet;

pub mod activity;
pub mod control;
pub mod definitions;
pub mod facts;
pub mod queries;
pub mod samples;
#[cfg(test)]
mod tests;

const MAX_COLUMNS: usize = 128;
const MAX_WIRE_RECORD_BYTES: usize = 1024 * 1024;

#[derive(Clone, Copy, Serialize)]
pub struct Field {
    name: &'static str,
    pointer: &'static str,
    kind: ScalarType,
    nullable: bool,
}
impl Field {
    pub const fn required(name: &'static str, pointer: &'static str, kind: ScalarType) -> Self {
        Self { name, pointer, kind, nullable: false }
    }
    pub const fn optional(name: &'static str, pointer: &'static str, kind: ScalarType) -> Self {
        Self { name, pointer, kind, nullable: true }
    }
    fn has_presence(self) -> bool {
        self.nullable && self.kind == ScalarType::String
    }
}
#[derive(Clone)]
pub struct OutputColumn {
    pub name: String,
    pub kind: ScalarType,
    pub nullable: bool,
}
pub struct Schema {
    table: &'static str,
    fields: Vec<Field>,
    columns: Vec<OutputColumn>,
    digest: String,
}
impl Schema {
    pub fn new(table: &'static str, fields: impl IntoIterator<Item = Field>) -> Result<Self> {
        if !identifier(table) {
            return Err(DeliveryProblem::InvalidContract.error());
        }
        let fields: Vec<_> = fields.into_iter().take(MAX_COLUMNS + 1).collect();
        if fields.is_empty()
            || fields.len() > MAX_COLUMNS
            || fields.iter().any(|field| !identifier(field.name) || !field.pointer.starts_with('/'))
        {
            return Err(DeliveryProblem::InvalidContract.error());
        }
        let mut columns = Vec::new();
        for field in &fields {
            if field.has_presence() {
                columns.push(OutputColumn {
                    name: format!("Has{}", field.name),
                    kind: ScalarType::Bool,
                    nullable: false,
                });
            }
            columns.push(OutputColumn {
                name: field.name.into(),
                kind: field.kind,
                nullable: field.nullable && field.kind != ScalarType::String,
            });
        }
        let mut names = BTreeSet::new();
        if columns.len() > MAX_COLUMNS || columns.iter().any(|column| !names.insert(&column.name)) {
            return Err(DeliveryProblem::InvalidContract.error());
        }
        let bytes = serde_json::to_vec(&(table, &fields, projection::PROJECTION_NORMALIZATION_VERSION))
            .map_err(|_| DeliveryProblem::Integrity.error())?;
        Ok(Self { table, fields, columns, digest: culprit_core::telemetry::digest(&bytes) })
    }
    pub fn table(&self) -> &str {
        self.table
    }
    pub fn digest(&self) -> &str {
        &self.digest
    }
    pub fn columns(&self) -> &[OutputColumn] {
        &self.columns
    }
    pub fn project(&self, source: &Value) -> Result<Vec<Value>> {
        let mut values = Vec::with_capacity(self.columns.len());
        for field in &self.fields {
            let value = source.pointer(field.pointer).unwrap_or(&Value::Null);
            if field.has_presence() {
                values.push(Value::Bool(!value.is_null()));
                values.push(if value.is_null() {
                    Value::String(String::new())
                } else {
                    field.kind.source(value, false)?
                });
            } else {
                values.push(field.kind.source(value, field.nullable)?);
            }
        }
        Ok(values)
    }
    pub fn returned_digest(&self, values: &[Value]) -> Result<[u8; 32]> {
        if values.len() != self.columns.len() {
            return Err(DeliveryProblem::Integrity.error());
        }
        let normalized = self
            .columns
            .iter()
            .zip(values)
            .map(|(column, value)| column.kind.returned(value, column.nullable))
            .collect::<Result<Vec<_>>>()?;
        projection::digest(&normalized)
    }
    pub fn encode(&self, values: &[Value]) -> Result<Vec<u8>> {
        if values.len() != self.columns.len() {
            return Err(DeliveryProblem::Integrity.error());
        }
        let object = self
            .columns
            .iter()
            .zip(values)
            .map(|(column, value)| (column.name.clone(), value.clone()))
            .collect::<serde_json::Map<_, _>>();
        let mut bytes = serde_json::to_vec(&object).map_err(|_| DeliveryProblem::Integrity.error())?;
        if bytes.len() >= MAX_WIRE_RECORD_BYTES {
            return Err(DeliveryProblem::Capacity.error());
        }
        bytes.push(b'\n');
        Ok(bytes)
    }
    pub fn verification_columns(&self) -> Vec<Column> {
        self.columns
            .iter()
            .map(|column| Column {
                column_name: column.name.clone(),
                column_type: column.kind.verification_type().into(),
            })
            .collect()
    }
    pub fn verification_projection(&self) -> String {
        self.columns
            .iter()
            .map(|column| {
                if column.kind.verification_type() == "string" && column.kind != ScalarType::String {
                    format!("{}=tostring({})", column.name, column.name)
                } else {
                    column.name.clone()
                }
            })
            .collect::<Vec<_>>()
            .join(", ")
    }
    pub fn create_table(&self) -> String {
        format!(
            ".create table {} ({})",
            self.table,
            self.columns
                .iter()
                .map(|column| format!("{}:{}", column.name, column.kind.kusto_type()))
                .collect::<Vec<_>>()
                .join(", ")
        )
    }
    pub fn mapping_name(&self) -> String {
        format!("{}Mapping", self.table)
    }
    pub fn create_mapping(&self) -> Result<String> {
        let mapping = self.columns.iter().map(|column| json!({"column":column.name,"datatype":column.kind.kusto_type(),"Properties":{"Path":format!("$.{}",column.name)}})).collect::<Vec<_>>();
        let text = serde_json::to_string(&mapping).map_err(|_| DeliveryProblem::Integrity.error())?;
        Ok(format!(".create table {} ingestion json mapping '{}' '{}'", self.table, self.mapping_name(), text))
    }
}
fn identifier(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name.as_bytes()[0].is_ascii_alphabetic()
        && name.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}
