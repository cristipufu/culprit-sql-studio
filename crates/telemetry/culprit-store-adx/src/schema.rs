//! One declaration drives typed projection, deployment mappings and exact verification columns.
use super::{
    projection::{self, ScalarType},
    response::Column,
};
use culprit_core::{Result, telemetry::DeliveryProblem};
use serde_json::{Value, json};
use std::collections::BTreeSet;

#[cfg(test)]
mod tests;

const MAX_COLUMNS: usize = 128;

use crate::projection::KustoType;
pub use culprit_collector::analytical::schema::{Field, OutputColumn};
use culprit_collector::analytical::{
    RowProjection,
    schema::{LogicalSchema, encode_row},
};
fn has_presence(field: Field) -> bool {
    field.nullable && field.kind == ScalarType::String
}
pub struct Schema {
    table: &'static str,
    fields: Vec<Field>,
    columns: Vec<OutputColumn>,
    digest: String,
}
impl Schema {
    pub fn new(table: &'static str, fields: impl IntoIterator<Item = Field>) -> Result<Self> {
        Self::from_logical(LogicalSchema::new(table, fields)?)
    }
    pub fn from_logical(logical: LogicalSchema) -> Result<Self> {
        let table = logical.table();
        let fields = logical.fields().to_vec();
        let mut columns = Vec::new();
        for field in &fields {
            if has_presence(*field) {
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
        let bytes = serde_json::to_vec(&(logical.digest(), "adx-json-presence-100ns-v1"))
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
            if has_presence(*field) {
                values.push(Value::Bool(!value.is_null()));
                values.push(if value.is_null() {
                    Value::String(String::new())
                } else {
                    projection::source(field.kind, value, false)?
                });
            } else {
                values.push(projection::source(field.kind, value, field.nullable)?);
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
            .map(|(column, value)| projection::returned(column.kind, value, column.nullable))
            .collect::<Result<Vec<_>>>()?;
        projection::digest(&normalized)
    }
    pub fn encode(&self, values: &[Value]) -> Result<Vec<u8>> {
        encode_row(&self.columns, values)
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
impl RowProjection for Schema {
    fn from_logical(schema: LogicalSchema) -> Result<Self> {
        Self::from_logical(schema)
    }
    fn table(&self) -> &str {
        self.table()
    }
    fn digest(&self) -> &str {
        self.digest()
    }
    fn columns(&self) -> &[OutputColumn] {
        self.columns()
    }
    fn project(&self, source: &Value) -> Result<Vec<Value>> {
        self.project(source)
    }
    fn encode(&self, values: &[Value]) -> Result<Vec<u8>> {
        self.encode(values)
    }
    fn returned_digest(&self, values: &[Value]) -> Result<[u8; 32]> {
        self.returned_digest(values)
    }
}
