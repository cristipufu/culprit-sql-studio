//! A compact projection of separately timed catalog context. SQL bodies and original evidence tokens stay outside.
use culprit_core::{
    Plan,
    telemetry::{MssqlCurrentTableType, MssqlCurrentTypeIndex, MssqlCurrentTypeSchema, MssqlCurrentTypeSchemaStatus},
};
use serde_json::{Value, json};

const MAX_TYPES: usize = 3;
const MAX_COLUMNS: usize = 16;
const MAX_INDEXES: usize = 4;
const MAX_INDEX_COLUMNS: usize = 8;

pub(super) fn value(plan: &Plan) -> Option<Value> {
    let words = culprit_engine::plan_type_schema(plan)?;
    let schema = plan.facts().current_type_schema.as_deref()?;
    let available = schema.status == MssqlCurrentTypeSchemaStatus::Available;
    Some(json!({
        "title": words.title,
        "summary": words.summary,
        "limitations": words.limitations,
        "status": schema.status,
        "observed_at": schema.observed_at,
        "type_count": schema.types.len(),
        "types_truncated": schema.types.len() > MAX_TYPES,
        "types": schema.types.iter().take(if available { MAX_TYPES } else { 0 })
            .map(|data_type| table_type(data_type, schema)).collect::<Vec<_>>(),
    }))
}

fn table_type(data_type: &MssqlCurrentTableType, schema: &MssqlCurrentTypeSchema) -> Value {
    json!({
            "schema": data_type.schema,
            "name": data_type.name,
            "parameters": schema.parameters.iter().filter(|parameter| parameter.user_type_id == data_type.user_type_id)
                .map(|parameter| &parameter.name).collect::<Vec<_>>(),
            "memory_optimized": data_type.memory_optimized,
            "column_count": data_type.columns.len(),
            "columns_truncated": data_type.columns.len() > MAX_COLUMNS,
            "columns": data_type.columns.iter().take(MAX_COLUMNS).map(|column| json!({
                "name": column.name,
                "type_schema": column.type_schema,
                "type_name": column.type_name,
                "max_length_bytes": column.max_length,
                "precision": column.precision,
                "scale": column.scale,
                "nullable": column.nullable,
                "collation": column.collation,
            })).collect::<Vec<_>>(),
            "index_count": data_type.indexes.len(),
            "indexes_truncated": data_type.indexes.len() > MAX_INDEXES,
            "indexes": data_type.indexes.iter().take(MAX_INDEXES).map(index).collect::<Vec<_>>(),
    })
}

fn index(index: &MssqlCurrentTypeIndex) -> Value {
    json!({
                "name": index.name,
                "native_type": index.native_type,
                "unique": index.unique,
                "primary_key": index.primary_key,
                "unique_constraint": index.unique_constraint,
                "disabled": index.disabled,
                "hypothetical": index.hypothetical,
                "columns_truncated": index.columns.len() > MAX_INDEX_COLUMNS,
                "columns": index.columns.iter().take(MAX_INDEX_COLUMNS).collect::<Vec<_>>(),
    })
}

#[cfg(test)]
mod tests;
