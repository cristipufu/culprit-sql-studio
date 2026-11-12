//! Logical analytical fields shared by every warehouse. Physical layouts belong to the destination adapter.
use super::projection::{self, ScalarType};
use culprit_core::{Result, telemetry::DeliveryProblem};
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeSet;

pub mod activity;
pub mod control;
pub mod definitions;
pub mod facts;
pub mod queries;
pub mod samples;

pub const MAX_COLUMNS: usize = 128;
pub const MAX_WIRE_RECORD_BYTES: usize = 1024 * 1024;

#[derive(Clone, Copy, Serialize)]
pub struct Field {
    pub name: &'static str,
    pub pointer: &'static str,
    pub kind: ScalarType,
    pub nullable: bool,
}
impl Field {
    pub const fn required(name: &'static str, pointer: &'static str, kind: ScalarType) -> Self {
        Self { name, pointer, kind, nullable: false }
    }
    pub const fn optional(name: &'static str, pointer: &'static str, kind: ScalarType) -> Self {
        Self { name, pointer, kind, nullable: true }
    }
}
#[derive(Clone)]
pub struct OutputColumn {
    pub name: String,
    pub kind: ScalarType,
    pub nullable: bool,
}
pub struct LogicalSchema {
    table: &'static str,
    fields: Vec<Field>,
    digest: String,
}
impl LogicalSchema {
    pub fn new(table: &'static str, fields: impl IntoIterator<Item = Field>) -> Result<Self> {
        let fields: Vec<_> = fields.into_iter().take(MAX_COLUMNS + 1).collect();
        let mut names = BTreeSet::new();
        if !identifier(table)
            || fields.is_empty()
            || fields.len() > MAX_COLUMNS
            || fields.iter().any(|f| !identifier(f.name) || !f.pointer.starts_with('/') || !names.insert(f.name))
        {
            return Err(DeliveryProblem::InvalidContract.error());
        }
        let bytes = serde_json::to_vec(&(
            table,
            &fields,
            projection::PROJECTION_NORMALIZATION_VERSION,
            super::prepared::PREPARATION_VERSION,
        ))
        .map_err(|_| DeliveryProblem::Integrity.error())?;
        Ok(Self { table, fields, digest: culprit_core::telemetry::digest(&bytes) })
    }
    pub fn table(&self) -> &'static str {
        self.table
    }
    pub fn digest(&self) -> &str {
        &self.digest
    }
    pub fn fields(&self) -> &[Field] {
        &self.fields
    }
    pub fn columns(&self) -> Vec<OutputColumn> {
        self.fields.iter().map(|f| OutputColumn { name: f.name.into(), kind: f.kind, nullable: f.nullable }).collect()
    }
    pub fn project(&self, source: &Value) -> Result<Vec<Value>> {
        self.fields
            .iter()
            .map(|f| f.kind.source(source.pointer(f.pointer).unwrap_or(&Value::Null), f.nullable))
            .collect()
    }
    pub fn encode(&self, values: &[Value]) -> Result<Vec<u8>> {
        encode_row(&self.columns(), values)
    }
}
/// JSON containers remain exact JSON text in physical storage; typed analytical views can parse them after proof.
pub fn encode_row(columns: &[OutputColumn], values: &[Value]) -> Result<Vec<u8>> {
    if columns.len() != values.len() {
        return Err(DeliveryProblem::Integrity.error());
    }
    let object = columns
        .iter()
        .zip(values)
        .map(|(column, value)| {
            let value = if column.kind == ScalarType::JsonContainer && !value.is_null() {
                Value::String(serde_json::to_string(value).map_err(|_| DeliveryProblem::Integrity.error())?)
            } else {
                value.clone()
            };
            Ok((column.name.clone(), value))
        })
        .collect::<Result<serde_json::Map<_, _>>>()?;
    let mut bytes = serde_json::to_vec(&object).map_err(|_| DeliveryProblem::Integrity.error())?;
    if bytes.len() >= MAX_WIRE_RECORD_BYTES {
        return Err(DeliveryProblem::Capacity.error());
    }
    bytes.push(b'\n');
    Ok(bytes)
}
pub fn identifier(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name.as_bytes()[0].is_ascii_alphabetic()
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}
