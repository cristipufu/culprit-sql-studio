//! Optional persisted catalog fields compile only after their native layout and permissions are observed.
use crate::{
    read::{invalid_data, optional_integer},
    value::Rec,
};
use culprit_core::{Result, telemetry::*};
use serde::Deserialize;

pub(super) struct QueryParts {
    pub before_configuration: String,
    pub before_ordinals: String,
    pub after_ordinals: String,
}

pub(super) fn query(parts: QueryParts) -> String {
    let capabilities = [
        capability("compression", "sys.indexes", "compression_delay", "int", 13, true),
        capability("ordering", "sys.index_columns", "column_store_order_ordinal", "tinyint", 16, true),
        capability("clustering", "sys.index_columns", "data_clustering_ordinal", "tinyint", 17, false),
    ]
    .join("\n");
    let before = crate::catalog_visibility::sql_literal(&format!(
        "{}\n{}",
        crate::statement_origin::OWN_QUERY_MARKER,
        parts.before_configuration
    ));
    let columns = crate::catalog_visibility::sql_literal(&parts.before_ordinals);
    let after = crate::catalog_visibility::sql_literal(&parts.after_ordinals);
    format!(
        r#"DECLARE /* culprit */ @culprit_index_major int=TRY_CONVERT(int,SERVERPROPERTY('ProductMajorVersion')),
 @culprit_index_edition int=TRY_CONVERT(int,SERVERPROPERTY('EngineEdition')),
 @culprit_index_metadata int=HAS_PERMS_BY_NAME(DB_NAME(),N'DATABASE',N'VIEW DEFINITION');
{capabilities}
DECLARE /* culprit */ @culprit_index_sql nvarchar(max)={before}
 +CASE WHEN @culprit_compression=0 THEN N'i.compression_delay' ELSE N'CAST(NULL AS int)' END
 +N' AS compression_delay,@culprit_compression AS compression_availability,@culprit_ordering AS ordering_availability,@culprit_clustering AS clustering_availability,'
 +{columns}
 +CASE WHEN @culprit_ordering=0 THEN N'ic.column_store_order_ordinal' ELSE N'CAST(NULL AS tinyint)' END
 +N' AS column_store_order_ordinal,'
 +CASE WHEN @culprit_clustering=0 THEN N'ic.data_clustering_ordinal' ELSE N'CAST(NULL AS tinyint)' END
 +N' AS data_clustering_ordinal'
 +{after};
EXEC /* culprit */ sys.sp_executesql @culprit_index_sql,
 N'@P1 int,@culprit_compression int,@culprit_ordering int,@culprit_clustering int',
 @P1=@P1,@culprit_compression=@culprit_compression,@culprit_ordering=@culprit_ordering,@culprit_clustering=@culprit_clustering;"#
    )
}

fn capability(
    variable: &str,
    catalog: &str,
    column: &str,
    native_type: &str,
    minimum_major: u32,
    azure: bool,
) -> String {
    let supported = if azure {
        format!("@culprit_index_major>={minimum_major} OR @culprit_index_edition IN(5,8)")
    } else {
        format!("@culprit_index_major>={minimum_major}")
    };
    format!(
        r#"DECLARE /* culprit */ @culprit_{variable} int=CASE
 WHEN @culprit_index_metadata=0 OR HAS_PERMS_BY_NAME(N'{catalog}',N'OBJECT',N'SELECT')=0 THEN 2
 WHEN @culprit_index_metadata IS NULL OR @culprit_index_major IS NULL OR @culprit_index_edition IS NULL
  OR OBJECT_ID(N'{catalog}') IS NULL OR HAS_PERMS_BY_NAME(N'{catalog}',N'OBJECT',N'SELECT') IS NULL THEN 3
 WHEN NOT({supported}) THEN 1
 WHEN NOT EXISTS(SELECT 1 FROM sys.all_columns WHERE object_id=OBJECT_ID(N'{catalog}') AND name=N'{column}') THEN 1
 WHEN NOT EXISTS(SELECT 1 FROM sys.all_columns WHERE object_id=OBJECT_ID(N'{catalog}') AND name=N'{column}' AND system_type_id=TYPE_ID(N'{native_type}')) THEN 3
 WHEN HAS_PERMS_BY_NAME(N'{catalog}',N'OBJECT',N'SELECT',N'{column}',N'COLUMN')=0 THEN 2
 WHEN HAS_PERMS_BY_NAME(N'{catalog}',N'OBJECT',N'SELECT',N'{column}',N'COLUMN')=1 THEN 0
 ELSE 3 END;"#
    )
}

pub(super) struct Columns {
    pub native: Vec<MssqlCatalogIndexColumn>,
    pub columnstore: Vec<MssqlColumnstoreColumn>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NativeColumn {
    index_column_id: u32,
    column_id: u32,
    name: String,
    key_ordinal: u32,
    descending: bool,
    included: bool,
    #[serde(deserialize_with = "required_nullable_ordinal")]
    column_store_order_ordinal: Option<u8>,
    #[serde(deserialize_with = "required_nullable_ordinal")]
    data_clustering_ordinal: Option<u8>,
}

fn required_nullable_ordinal<'de, D: serde::Deserializer<'de>>(reader: D) -> std::result::Result<Option<u8>, D::Error> {
    Option::<u8>::deserialize(reader)
}

pub(super) fn columns(raw: &str) -> Result<Columns> {
    let values: Vec<NativeColumn> = serde_json::from_str(raw).map_err(|_| invalid_data())?;
    if values.len() > MAX_MSSQL_INDEX_COLUMNS {
        return Err(invalid_data());
    }
    let mut native = Vec::with_capacity(values.len());
    let mut columnstore = Vec::with_capacity(values.len());
    for value in values {
        columnstore.push(MssqlColumnstoreColumn {
            index_column_id: value.index_column_id,
            column_id: value.column_id,
            name: value.name.clone(),
            column_store_order_ordinal: value.column_store_order_ordinal,
            data_clustering_ordinal: value.data_clustering_ordinal,
        });
        native.push(MssqlCatalogIndexColumn {
            index_column_id: value.index_column_id,
            column_id: value.column_id,
            name: value.name,
            key_ordinal: value.key_ordinal,
            descending: value.descending,
            included: value.included,
        });
    }
    Ok(Columns { native, columnstore })
}

pub(super) fn configuration(
    row: &Rec,
    index: &MssqlIndexSnapshot,
    columns: Vec<MssqlColumnstoreColumn>,
) -> Result<Option<Box<MssqlColumnstoreConfiguration>>> {
    let compression_delay_availability = crate::catalog_visibility::availability(row, "compression_availability")?;
    let compression_delay_minutes = optional_integer(row, "compression_delay")?
        .map(|value| u32::try_from(value).map_err(|_| invalid_data()))
        .transpose()?;
    let column_store_order_availability = crate::catalog_visibility::availability(row, "ordering_availability")?;
    let data_clustering_availability = crate::catalog_visibility::availability(row, "clustering_availability")?;
    let configuration = MssqlColumnstoreConfiguration {
        table: index.table.clone(),
        index_id: index.index_id,
        schema: index.schema.clone(),
        table_name: index.table_name.clone(),
        name: index.name.clone(),
        table_modified_without_offset: index.table_modified_without_offset.clone(),
        compression_delay_availability,
        compression_delay_minutes,
        column_store_order_availability,
        data_clustering_availability,
        columns,
    };
    if !configuration.is_valid() {
        return Err(invalid_data());
    }
    Ok(matches!(index.type_code, 5 | 6).then(|| Box::new(configuration)))
}

#[cfg(test)]
mod tests;
