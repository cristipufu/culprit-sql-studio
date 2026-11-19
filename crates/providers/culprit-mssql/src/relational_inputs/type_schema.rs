//! Bounded current catalog metadata shares the original payload dispatch, without supplying compilation facts.
use crate::{
    read::{invalid_data, optional_boolean, optional_integer, optional_text},
    value::Rec,
};
use culprit_core::{Result, telemetry::*};

pub(crate) const NATIVE_JSON_BYTES: usize = 32 * 1024;
pub(crate) const OUTPUT_COLUMNS: &str =
    "current_type_observed_at,current_type_visibility,current_type_bytes,current_type_schema";
const ALIAS: &str = "culprit_current_types";

pub(crate) fn columns() -> String {
    format!(
        "{ALIAS}.observed_at AS current_type_observed_at,{ALIAS}.visible AS current_type_visibility,CAST(DATALENGTH({ALIAS}.payload) AS bigint) AS current_type_bytes,CASE WHEN {ALIAS}.visible=1 AND DATALENGTH({ALIAS}.payload)<={NATIVE_JSON_BYTES} THEN {ALIAS}.payload END AS current_type_schema"
    )
}

pub(crate) fn uncollected_columns() -> &'static str {
    "CONVERT(varchar(40),SYSUTCDATETIME(),127)+'Z' AS current_type_observed_at,CAST(NULL AS int) AS current_type_visibility,CAST(NULL AS bigint) AS current_type_bytes,CAST(NULL AS nvarchar(max)) AS current_type_schema"
}

pub(crate) fn relation(text_alias: &str) -> String {
    let parameters = parameters(text_alias);
    let types = types(text_alias);
    format!(
        r#"OUTER APPLY(SELECT CONVERT(varchar(40),SYSUTCDATETIME(),127)+'Z' AS observed_at,
 HAS_PERMS_BY_NAME(DB_NAME(),'DATABASE','VIEW DEFINITION') AS visible,
 (SELECT JSON_QUERY((SELECT DB_ID() AS [identity.database_id],m.object_id AS [identity.object_id],
 CONVERT(varchar(40),m.create_date,126) AS [identity.created_without_offset],
 s.name AS [schema],m.name,CONVERT(varchar(40),m.modify_date,126) AS modified_without_offset
 FROM sys.objects m JOIN sys.schemas s ON s.schema_id=m.schema_id
 WHERE {text_alias}.dbid=DB_ID() AND m.object_id={text_alias}.objectid AND m.type='P'
 FOR JSON PATH,WITHOUT_ARRAY_WRAPPER)) AS module,
 JSON_QUERY(({parameters})) AS parameters,JSON_QUERY(({types})) AS types
 FOR JSON PATH,WITHOUT_ARRAY_WRAPPER,INCLUDE_NULL_VALUES) AS payload
 WHERE {text_alias}.dbid=DB_ID() AND {text_alias}.objectid>0) {ALIAS}"#
    )
}

fn parameters(text_alias: &str) -> String {
    let limit = MAX_MSSQL_CURRENT_TYPE_PARAMETERS + 1;
    format!(
        "SELECT TOP({limit}) p.parameter_id,p.name,p.user_type_id FROM sys.parameters p JOIN sys.table_types tt ON tt.user_type_id=p.user_type_id WHERE {text_alias}.dbid=DB_ID() AND p.object_id={text_alias}.objectid AND p.is_readonly=1 ORDER BY p.parameter_id FOR JSON PATH"
    )
}

fn types(text_alias: &str) -> String {
    let parameter_limit = MAX_MSSQL_CURRENT_TYPE_PARAMETERS + 1;
    let type_limit = MAX_MSSQL_CURRENT_TYPES + 1;
    let column_limit = MAX_MSSQL_CURRENT_TYPE_COLUMNS + 1;
    let index_limit = MAX_MSSQL_CURRENT_TYPE_INDEXES + 1;
    let index_column_limit = MAX_MSSQL_CURRENT_TYPE_INDEX_COLUMNS + 1;
    format!(
        r#"SELECT TOP({type_limit}) tt.schema_id,tt.user_type_id,s.name AS [schema],tt.name,
 DB_ID() AS [backing_object.database_id],o.object_id AS [backing_object.object_id],
 CONVERT(varchar(40),o.create_date,126) AS [backing_object.created_without_offset],
 CONVERT(varchar(40),o.modify_date,126) AS modified_without_offset,tt.is_memory_optimized AS memory_optimized,
 JSON_QUERY((SELECT TOP({column_limit}) c.column_id,c.name,c.user_type_id,c.system_type_id,
 cs.name AS type_schema,ct.name AS type_name,c.max_length,c.precision,c.scale,
 c.is_nullable AS nullable,c.is_identity AS [identity],c.is_computed AS computed,c.collation_name AS collation
 FROM sys.columns c JOIN sys.types ct ON ct.user_type_id=c.user_type_id
 JOIN sys.schemas cs ON cs.schema_id=ct.schema_id WHERE c.object_id=tt.type_table_object_id
 ORDER BY c.column_id FOR JSON PATH,INCLUDE_NULL_VALUES)) AS columns,
 JSON_QUERY((SELECT TOP({index_limit}) i.index_id,i.name,i.type AS type_code,i.type_desc AS native_type,
 i.is_unique AS [unique],i.is_primary_key AS primary_key,i.is_unique_constraint AS unique_constraint,
 i.is_disabled AS disabled,i.is_hypothetical AS hypothetical,
 JSON_QUERY((SELECT TOP({index_column_limit}) ic.index_column_id,ic.column_id,c.name,ic.key_ordinal,
 ic.is_descending_key AS descending,ic.is_included_column AS included
 FROM sys.index_columns ic JOIN sys.columns c ON c.object_id=ic.object_id AND c.column_id=ic.column_id
 WHERE ic.object_id=i.object_id AND ic.index_id=i.index_id
 ORDER BY ic.index_column_id FOR JSON PATH)) AS columns
 FROM sys.indexes i WHERE i.object_id=tt.type_table_object_id AND i.index_id>0
 ORDER BY i.index_id FOR JSON PATH)) AS indexes
 FROM sys.table_types tt JOIN sys.schemas s ON s.schema_id=tt.schema_id
 JOIN sys.objects o ON o.object_id=tt.type_table_object_id
 WHERE {text_alias}.dbid=DB_ID() AND tt.user_type_id IN
 (SELECT TOP({parameter_limit}) p.user_type_id FROM sys.parameters p
 WHERE p.object_id={text_alias}.objectid AND p.is_readonly=1 ORDER BY p.parameter_id)
 ORDER BY tt.user_type_id FOR JSON PATH"#
    )
}

pub(crate) fn decode(
    row: &Rec,
    context: &CachedStatementContext,
    original: Option<&MssqlModuleParameters>,
) -> Result<Option<MssqlCurrentTypeSchema>> {
    let Some(original) =
        original.filter(|original| original.is_valid_for(context) && !original.table_parameters.is_empty())
    else {
        return Ok(None);
    };
    let Some(observed_at) = optional_text(row, "current_type_observed_at")? else { return Ok(None) };
    let unavailable = |status| {
        let value = MssqlCurrentTypeSchema::unavailable(observed_at.clone(), status);
        value.is_valid_for(context, Some(original)).then_some(Some(value)).ok_or_else(invalid_data)
    };
    let bytes = optional_integer(row, "current_type_bytes")?;
    if bytes.is_none() && optional_text(row, "current_type_schema")?.is_none() {
        return unavailable(MssqlCurrentTypeSchemaStatus::NotCollected);
    }
    if optional_boolean(row, "current_type_visibility")? != Some(true) {
        return unavailable(MssqlCurrentTypeSchemaStatus::Restricted);
    }
    if original.table_parameters.values().any(|parts| parts.len() != 2) {
        return unavailable(MssqlCurrentTypeSchemaStatus::NotCollected);
    }
    if original.table_parameters.len() > MAX_MSSQL_CURRENT_TYPE_PARAMETERS {
        return unavailable(MssqlCurrentTypeSchemaStatus::Oversized);
    }
    let Some(bytes) = bytes else {
        return unavailable(MssqlCurrentTypeSchemaStatus::Changed);
    };
    let bytes = usize::try_from(bytes).map_err(|_| invalid_data())?;
    if bytes > NATIVE_JSON_BYTES {
        return unavailable(MssqlCurrentTypeSchemaStatus::Oversized);
    }
    let Some(json) = optional_text(row, "current_type_schema")? else {
        return unavailable(MssqlCurrentTypeSchemaStatus::Changed);
    };
    if json.len() > MAX_MSSQL_CURRENT_TYPE_SCHEMA_BYTES || json.encode_utf16().count() * 2 != bytes {
        return Err(invalid_data());
    }
    let mut value: serde_json::Value = serde_json::from_str(&json).map_err(|_| invalid_data())?;
    if overflowed(&value) {
        return unavailable(MssqlCurrentTypeSchemaStatus::Oversized);
    }
    let object = value.as_object_mut().ok_or_else(invalid_data)?;
    object.insert("observed_at".into(), observed_at.clone().into());
    object.insert("status".into(), "available".into());
    let schema: MssqlCurrentTypeSchema = serde_json::from_value(value).map_err(|_| invalid_data())?;
    if schema.module.is_none() || schema.parameters.is_empty() || schema.types.is_empty() {
        return unavailable(MssqlCurrentTypeSchemaStatus::Changed);
    }
    if !schema.is_valid() {
        return Err(invalid_data());
    }
    if !schema.is_valid_for(context, Some(original)) {
        return unavailable(MssqlCurrentTypeSchemaStatus::Changed);
    }
    Ok(Some(schema))
}

fn overflowed(value: &serde_json::Value) -> bool {
    let length = |value: &serde_json::Value, field: &str| {
        value.get(field).and_then(serde_json::Value::as_array).map(Vec::len).unwrap_or(0)
    };
    length(value, "parameters") > MAX_MSSQL_CURRENT_TYPE_PARAMETERS
        || length(value, "types") > MAX_MSSQL_CURRENT_TYPES
        || value.get("types").and_then(serde_json::Value::as_array).is_some_and(|types| {
            types.iter().any(|data_type| {
                length(data_type, "columns") > MAX_MSSQL_CURRENT_TYPE_COLUMNS
                    || length(data_type, "indexes") > MAX_MSSQL_CURRENT_TYPE_INDEXES
                    || data_type.get("indexes").and_then(serde_json::Value::as_array).is_some_and(|indexes| {
                        indexes.iter().map(|index| length(index, "columns")).sum::<usize>()
                            > MAX_MSSQL_CURRENT_TYPE_INDEX_COLUMNS
                    })
            })
        })
}

#[cfg(test)]
pub(crate) mod tests;
