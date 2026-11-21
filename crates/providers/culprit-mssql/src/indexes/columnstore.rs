//! Optional persisted catalog fields compile only after their native layout and permissions are observed.
use crate::catalog_visibility::index_column_capability as capability;
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
    let configuration = super::configuration::columns_expression(true, "o");
    let configuration_declarations = super::configuration::declarations(true);
    let configuration_parameters = super::configuration::parameters(true);
    let configuration_arguments = super::configuration::arguments(true);
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
{configuration_declarations}
DECLARE /* culprit */ @culprit_index_sql nvarchar(max)=CAST({before} AS nvarchar(max))
 +{configuration}+N','
 +CASE WHEN @culprit_compression=0 THEN N'i.compression_delay' ELSE N'CAST(NULL AS int)' END
 +N' AS compression_delay,@culprit_compression AS compression_availability,@culprit_ordering AS ordering_availability,@culprit_clustering AS clustering_availability,'
 +{columns}
 +CASE WHEN @culprit_ordering=0 THEN N'ic.column_store_order_ordinal' ELSE N'CAST(NULL AS tinyint)' END
 +N' AS column_store_order_ordinal,'
 +CASE WHEN @culprit_clustering=0 THEN N'ic.data_clustering_ordinal' ELSE N'CAST(NULL AS tinyint)' END
 +N' AS data_clustering_ordinal'
 +{after};
EXEC /* culprit */ sys.sp_executesql @culprit_index_sql,
 N'@P1 int,@culprit_compression int,@culprit_ordering int,@culprit_clustering int{configuration_parameters}',
 @P1=@P1,@culprit_compression=@culprit_compression,@culprit_ordering=@culprit_ordering,@culprit_clustering=@culprit_clustering{configuration_arguments};"#
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
        table: index.parent_object.clone(),
        index_id: index.index_id,
        schema: index.parent_schema.clone(),
        table_name: index.parent_name.clone(),
        name: index.name.clone(),
        table_modified_without_offset: index.parent_modified_without_offset.clone(),
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
