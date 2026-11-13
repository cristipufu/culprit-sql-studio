//! A dialect-neutral JSON physical layout for proof-reader fixtures, never a shipped warehouse adapter.
use crate::analytical::{
    RowProjection, projection,
    schema::{LogicalSchema, OutputColumn},
};
use culprit_core::{Result, telemetry::DeliveryProblem};
use serde_json::{Map, Value};

pub struct JsonProjection {
    logical: LogicalSchema,
    columns: Vec<OutputColumn>,
}
impl JsonProjection {
    pub fn logical_values(&self, row: &[Value]) -> Result<Map<String, Value>> {
        if row.len() != self.columns.len() {
            return Err(DeliveryProblem::Integrity.error());
        }
        self.columns
            .iter()
            .zip(row)
            .map(|(column, value)| Ok((column.name.clone(), column.kind.returned(value, column.nullable)?)))
            .collect()
    }
}
impl RowProjection for JsonProjection {
    fn from_logical(logical: LogicalSchema) -> Result<Self> {
        Ok(Self { columns: logical.columns(), logical })
    }
    fn table(&self) -> &str {
        self.logical.table()
    }
    fn digest(&self) -> &str {
        self.logical.digest()
    }
    fn columns(&self) -> &[OutputColumn] {
        &self.columns
    }
    fn project(&self, source: &Value) -> Result<Vec<Value>> {
        self.logical.project(source)
    }
    fn encode(&self, values: &[Value]) -> Result<Vec<u8>> {
        self.logical.encode(values)
    }
    fn returned_digest(&self, row: &[Value]) -> Result<[u8; 32]> {
        let values = self.logical_values(row)?;
        projection::digest(&self.columns.iter().map(|column| values[&column.name].clone()).collect::<Vec<_>>())
    }
}
