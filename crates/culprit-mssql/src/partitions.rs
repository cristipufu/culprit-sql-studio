//! Bounded partition metadata, shared by the app and export. Raw converted values never enter serialized facts.
use crate::{
    catalog_expression,
    read::{NativeRead, Parameter, ReadError, boolean, integer, invalid_data, optional_integer, optional_text, text},
    value::Rec,
};
use culprit_core::{Result, telemetry::*};
const MAX_APPLICATION_BOUNDARIES: usize = 20_000;
const MAX_NATIVE_HASH_BYTES: usize = 8_000; // HASHBYTES input ceiling on supported pre-2016 SQL Server layouts.
#[derive(Clone, Copy)]
pub(crate) enum Selection {
    BaseStorage,
    AllIndexes,
}
pub(crate) struct Boundary {
    pub definition: MssqlPartitionBoundary,
    pub raw_display_value: Option<String>,
}
pub(crate) fn select_sql(selection: Selection, maximum_bytes: usize) -> String {
    let predicate = match selection {
        Selection::BaseStorage => "AND i.index_id IN(0,1)",
        Selection::AllIndexes => "",
    };
    let display = catalog_expression::select("display.value", "boundary", maximum_bytes);
    format!(
        r#"SELECT TOP (@P1) DB_ID() AS database_id,t.object_id,s.name AS schema_name,t.name AS table_name,
 CONVERT(varchar(40),t.create_date,126) AS created_without_offset,
 i.index_id,i.name AS index_name,i.is_disabled AS index_disabled,i.is_hypothetical AS index_hypothetical,
 ps.data_space_id AS scheme_id,ps.name AS scheme_name,pf.function_id,pf.name AS function_name,
 CONVERT(varchar(40),pf.create_date,126) AS function_created_without_offset,
 CONVERT(varchar(40),pf.modify_date,126) AS function_modified_without_offset,
 pf.fanout,pf.boundary_value_on_right,pp.parameter_id,ts.name AS parameter_type_schema,ty.name AS parameter_type,
 pp.system_type_id,pp.user_type_id,pp.max_length,pp.precision,pp.scale,pp.collation_name,
 prv.boundary_id,CASE WHEN prv.boundary_id IS NULL THEN NULL WHEN prv.value IS NULL THEN CAST(1 AS bit) ELSE CAST(0 AS bit) END AS value_is_null,
 CONVERT(nvarchar(128),SQL_VARIANT_PROPERTY(prv.value,'BaseType')) AS native_value_type,
 CONVERT(varchar(64),HASHBYTES('SHA2_256',CASE WHEN DATALENGTH(native.value)<={MAX_NATIVE_HASH_BYTES} THEN native.value END),2) AS native_value_sha256,
 {display}
FROM sys.tables t JOIN sys.schemas s ON s.schema_id=t.schema_id
JOIN sys.indexes i ON i.object_id=t.object_id
JOIN sys.partition_schemes ps ON ps.data_space_id=i.data_space_id
JOIN sys.partition_functions pf ON pf.function_id=ps.function_id
JOIN sys.partition_parameters pp ON pp.function_id=pf.function_id
JOIN sys.types ty ON ty.user_type_id=pp.user_type_id JOIN sys.schemas ts ON ts.schema_id=ty.schema_id
LEFT JOIN sys.partition_range_values prv ON prv.function_id=pf.function_id AND prv.parameter_id=pp.parameter_id
CROSS APPLY(SELECT CONVERT(varbinary(max),prv.value) AS value) native
CROSS APPLY(SELECT CASE WHEN prv.boundary_id IS NULL THEN NULL WHEN prv.value IS NULL THEN CAST(N'NULL' AS nvarchar(max))
 WHEN CONVERT(nvarchar(128),SQL_VARIANT_PROPERTY(prv.value,'BaseType')) IN(N'binary',N'varbinary') THEN CONVERT(nvarchar(max),native.value,1)
 ELSE CAST(N'N''' AS nvarchar(max))+REPLACE(CONVERT(nvarchar(max),prv.value,126),N'''',N'''''')+N'''' END AS value) display
WHERE t.is_ms_shipped=0 {predicate}
ORDER BY t.object_id,i.index_id,pp.parameter_id,prv.boundary_id OPTION(MAXDOP 1)"#
    )
}
pub(crate) fn decode(row: &Rec, maximum_bytes: usize) -> Result<Boundary> {
    let narrow = |field| integer(row, field)?.try_into().map_err(|_| invalid_data());
    let boundary_id =
        optional_integer(row, "boundary_id")?.map(|id| id.try_into().map_err(|_| invalid_data())).transpose()?;
    let definition = MssqlPartitionBoundary {
        table: MssqlObjectIdentity {
            database_id: narrow("database_id")?,
            object_id: narrow("object_id")?,
            created_without_offset: crate::sql_time::native_time(&text(row, "created_without_offset")?)?,
        },
        schema: text(row, "schema_name")?,
        table_name: text(row, "table_name")?,
        index_id: narrow("index_id")?,
        index_name: optional_text(row, "index_name")?,
        index_disabled: boolean(row, "index_disabled")?,
        index_hypothetical: boolean(row, "index_hypothetical")?,
        scheme_id: narrow("scheme_id")?,
        scheme_name: text(row, "scheme_name")?,
        function_id: narrow("function_id")?,
        function_name: text(row, "function_name")?,
        function_created_without_offset: crate::sql_time::native_time(&text(row, "function_created_without_offset")?)?,
        function_modified_without_offset: crate::sql_time::native_time(&text(
            row,
            "function_modified_without_offset",
        )?)?,
        fanout: narrow("fanout")?,
        boundary_on_right: boolean(row, "boundary_value_on_right")?,
        parameter_id: integer(row, "parameter_id")?.try_into().map_err(|_| invalid_data())?,
        parameter_type_schema: text(row, "parameter_type_schema")?,
        parameter_type: text(row, "parameter_type")?,
        parameter_system_type_id: integer(row, "system_type_id")?.try_into().map_err(|_| invalid_data())?,
        parameter_user_type_id: narrow("user_type_id")?,
        parameter_maximum_bytes: integer(row, "max_length")?.try_into().map_err(|_| invalid_data())?,
        parameter_precision: integer(row, "precision")?.try_into().map_err(|_| invalid_data())?,
        parameter_scale: integer(row, "scale")?.try_into().map_err(|_| invalid_data())?,
        parameter_collation: optional_text(row, "collation_name")?,
        boundary_id,
        value_is_null: crate::read::optional_boolean(row, "value_is_null")?,
        native_value_type: optional_text(row, "native_value_type")?,
        native_value_sha256: optional_text(row, "native_value_sha256")?.map(|value| value.to_ascii_lowercase()),
        display_value: catalog_expression::decode_using(
            row,
            boundary_id.is_some(),
            "boundary",
            maximum_bytes,
            CatalogExpression::partition_bound,
        )?,
    };
    if !definition.is_valid() {
        return Err(invalid_data());
    }
    Ok(Boundary { definition, raw_display_value: optional_text(row, "boundary_value")? })
}
pub(crate) async fn application(reader: &mut impl NativeRead) -> Result<Vec<Boundary>> {
    let rows = reader
        .query(
            &select_sql(Selection::BaseStorage, MAX_CATALOG_EXPRESSION_BYTES),
            vec![Parameter::Integer((MAX_APPLICATION_BOUNDARIES + 1) as i64)],
            MAX_APPLICATION_BOUNDARIES + 1,
        )
        .await
        .map_err(ReadError::into_error)?;
    if rows.len() > MAX_APPLICATION_BOUNDARIES {
        return Err(CollectionStop::RowLimit.error());
    }
    let values = rows.iter().map(|row| decode(row, MAX_CATALOG_EXPRESSION_BYTES)).collect::<Result<Vec<_>>>()?;
    for value in &values {
        if value.definition.index_id > 1 {
            return Err(invalid_data());
        }
        if let Some(expression) = &value.definition.display_value {
            catalog_expression::require_available(expression)?;
        }
    }
    Ok(values)
}
pub(crate) fn application_bound(boundary: &Boundary) -> Option<String> {
    let id = boundary.definition.boundary_id?;
    let value = boundary.raw_display_value.as_deref()?;
    let comparison = if boundary.definition.boundary_on_right { "LESS THAN" } else { "LESS THAN OR EQUAL" };
    Some(format!("partition_bound=VALUES {comparison} ({value}):p{id}"))
}

#[cfg(test)]
pub(crate) mod tests;
