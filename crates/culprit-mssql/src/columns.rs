//! One bounded catalog projection supplies app columns and versioned export definitions.
use crate::{
    read::{NativeRead, Parameter, ReadError, boolean, integer, invalid_data, optional_text, signed_integer, text},
    value::Rec,
};
use culprit_core::{ColumnInfo, Result, telemetry::*};

const APPLICATION_COLUMNS: usize = 10_000;
pub(crate) fn select_sql(limit: usize) -> String {
    let default = crate::catalog_expression::select("dc.definition", "default", limit);
    let computed = crate::catalog_expression::select("cc.definition", "computed", limit);
    format!(
        r#"
SELECT TOP (@P1) DB_ID() AS database_id,t.object_id,
 CONVERT(varchar(40),t.create_date,126) AS created_without_offset,
 CONVERT(varchar(40),t.modify_date,126) AS modified_without_offset,
 s.name AS schema_name,t.name AS table_name,c.name AS column_name,c.column_id,
 c.user_type_id,c.system_type_id,ts.name AS type_schema,ty.name AS type_name,
 c.max_length,c.precision,c.scale,c.is_nullable,c.is_identity,c.is_computed,c.collation_name,
 c.default_object_id,{default},{computed}
FROM sys.columns c JOIN sys.tables t ON t.object_id=c.object_id
JOIN sys.schemas s ON s.schema_id=t.schema_id
JOIN sys.types ty ON ty.user_type_id=c.user_type_id
JOIN sys.schemas ts ON ts.schema_id=ty.schema_id
LEFT JOIN sys.default_constraints dc ON dc.object_id=c.default_object_id
LEFT JOIN sys.computed_columns cc ON cc.object_id=c.object_id AND cc.column_id=c.column_id
WHERE t.is_ms_shipped=0 ORDER BY t.object_id,c.column_id OPTION(MAXDOP 1)
"#
    )
}

pub(crate) struct NativeColumn {
    pub definition: MssqlColumnDefinition,
    default_sql: Option<String>,
}

pub(crate) fn decode(row: &Rec, limit: usize) -> Result<NativeColumn> {
    let narrow = |field| integer(row, field)?.try_into().map_err(|_| invalid_data());
    let default_present = integer(row, "default_object_id")? > 0;
    let definition = MssqlColumnDefinition {
        table: MssqlObjectIdentity {
            database_id: narrow("database_id")?,
            object_id: narrow("object_id")?,
            created_without_offset: text(row, "created_without_offset")?,
        },
        schema: text(row, "schema_name")?,
        table_name: text(row, "table_name")?,
        name: text(row, "column_name")?,
        column_id: narrow("column_id")?,
        table_modified_without_offset: text(row, "modified_without_offset")?,
        user_type_id: narrow("user_type_id")?,
        system_type_id: narrow("system_type_id")?,
        type_schema: text(row, "type_schema")?,
        type_name: text(row, "type_name")?,
        max_length: signed_integer(row, "max_length")?.try_into().map_err(|_| invalid_data())?,
        precision: integer(row, "precision")?.try_into().map_err(|_| invalid_data())?,
        scale: integer(row, "scale")?.try_into().map_err(|_| invalid_data())?,
        nullable: boolean(row, "is_nullable")?,
        identity: boolean(row, "is_identity")?,
        default: crate::catalog_expression::decode(row, default_present, "default", limit)?,
        computed: crate::catalog_expression::decode(row, boolean(row, "is_computed")?, "computed", limit)?,
        collation: optional_text(row, "collation_name")?,
    };
    if !definition.is_valid() {
        return Err(invalid_data());
    }
    Ok(NativeColumn { definition, default_sql: optional_text(row, "default_value")? })
}

pub(crate) async fn application<R: NativeRead>(reader: &mut R) -> Result<Vec<ColumnInfo>> {
    let rows = reader
        .query(
            &select_sql(MAX_CATALOG_EXPRESSION_BYTES),
            vec![Parameter::Integer((APPLICATION_COLUMNS + 1) as i64)],
            APPLICATION_COLUMNS + 1,
        )
        .await
        .map_err(ReadError::into_error)?;
    if rows.len() > APPLICATION_COLUMNS {
        return Err(CollectionStop::RowLimit.error());
    }
    rows.iter().map(|row| decode(row, MAX_CATALOG_EXPRESSION_BYTES).and_then(NativeColumn::project)).collect()
}
impl NativeColumn {
    fn project(self) -> Result<ColumnInfo> {
        let column = self.definition;
        if let Some(default) = &column.default {
            crate::catalog_expression::require_available(default)?;
        }
        Ok(ColumnInfo {
            data_type: declared_type(&column),
            generated: column.identity || column.computed.is_some(),
            schema: column.schema,
            table: column.table_name,
            name: column.name,
            nullable: column.nullable,
            default: self.default_sql,
            ordinal: column.column_id,
        })
    }
}

fn declared_type(column: &MssqlColumnDefinition) -> String {
    let name = &column.type_name;
    let length = column.max_length;
    match name.to_ascii_lowercase().as_str() {
        "varchar" | "char" | "varbinary" | "binary" if length < 0 => format!("{name}(max)"),
        "varchar" | "char" | "varbinary" | "binary" => format!("{name}({length})"),
        "nvarchar" | "nchar" if length < 0 => format!("{name}(max)"),
        "nvarchar" | "nchar" => format!("{name}({})", length / 2),
        "decimal" | "numeric" => format!("{name}({},{})", column.precision, column.scale),
        "datetime2" | "datetimeoffset" | "time" => format!("{name}({})", column.scale),
        _ => name.clone(),
    }
}

#[cfg(test)]
pub(crate) mod tests;
