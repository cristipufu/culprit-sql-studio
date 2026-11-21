//! The same bounded persisted catalog companion serves heap/table and named-index native projections.
use crate::{
    read::{invalid_data, optional_boolean, optional_integer, optional_text},
    value::Rec,
};
use culprit_core::{Result, telemetry::*};
const PARTITION_JSON_BYTES: usize = 32 * 1024;

#[derive(Clone, Copy)]
struct Field {
    name: &'static str,
    kind: &'static str,
    sql_type: &'static str,
    major: u32,
}
const OPTIONS: [Field; 5] = [
    Field { name: "fill_factor", kind: "tinyint", sql_type: "tinyint", major: 9 },
    Field { name: "is_padded", kind: "bit", sql_type: "bit", major: 9 },
    Field { name: "ignore_dup_key", kind: "bit", sql_type: "bit", major: 9 },
    Field { name: "allow_row_locks", kind: "bit", sql_type: "bit", major: 9 },
    Field { name: "allow_page_locks", kind: "bit", sql_type: "bit", major: 9 },
];
const COMPRESSION: [Field; 4] = [
    Field { name: "data_compression", kind: "tinyint", sql_type: "tinyint", major: 10 },
    Field { name: "data_compression_desc", kind: "nvarchar", sql_type: "nvarchar(60)", major: 10 },
    Field { name: "xml_compression", kind: "bit", sql_type: "bit", major: 16 },
    Field { name: "xml_compression_desc", kind: "varchar", sql_type: "varchar(3)", major: 16 },
];
impl Field {
    fn variable(self) -> String {
        format!("@culprit_cfg_{}", self.name)
    }
    fn availability_alias(self) -> String {
        format!("configuration_{}_availability", self.name)
    }
    fn capability(self, catalog: &str) -> String {
        crate::catalog_visibility::index_column_capability(
            &format!("cfg_{}", self.name),
            catalog,
            self.name,
            self.kind,
            self.major,
            true,
        )
    }
    fn value_expression(self, owner: &str, alias: &str) -> String {
        format!(
            "CASE WHEN {}=0 THEN N'{owner}.{}' ELSE N'CAST(NULL AS {})' END+N' AS {alias}'",
            self.variable(),
            self.name,
            self.sql_type
        )
    }
    fn availability_expression(self) -> String {
        format!("N'{} AS {}'", self.variable(), self.availability_alias())
    }
}
fn fields(options: bool) -> impl Iterator<Item = Field> {
    OPTIONS.into_iter().filter(move |_| options).chain(COMPRESSION)
}
pub(crate) fn declarations(options: bool) -> String {
    let fields = OPTIONS
        .into_iter()
        .filter(|_| options)
        .map(|field| field.capability("sys.indexes"))
        .chain(COMPRESSION.into_iter().map(|field| field.capability("sys.partitions")))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        r#"{fields}
DECLARE /* culprit */ @culprit_cfg_partitions int=CASE
 WHEN @culprit_index_metadata=0 OR HAS_PERMS_BY_NAME(N'sys.partitions',N'OBJECT',N'SELECT')=0 THEN 2
 WHEN @culprit_index_metadata IS NULL OR OBJECT_ID(N'sys.partitions') IS NULL
  OR HAS_PERMS_BY_NAME(N'sys.partitions',N'OBJECT',N'SELECT') IS NULL THEN 3
 WHEN (SELECT COUNT(*) FROM sys.all_columns WHERE object_id=OBJECT_ID(N'sys.partitions')
  AND ((name IN(N'object_id',N'index_id',N'partition_number') AND system_type_id=TYPE_ID(N'int'))
    OR (name IN(N'partition_id',N'hobt_id') AND system_type_id=TYPE_ID(N'bigint'))))<>5 THEN 1
 WHEN EXISTS(SELECT 1 FROM (VALUES(N'object_id'),(N'index_id'),(N'partition_number'),(N'partition_id'),(N'hobt_id')) v(name)
  WHERE HAS_PERMS_BY_NAME(N'sys.partitions',N'OBJECT',N'SELECT',v.name,N'COLUMN')=0) THEN 2
 WHEN EXISTS(SELECT 1 FROM (VALUES(N'object_id'),(N'index_id'),(N'partition_number'),(N'partition_id'),(N'hobt_id')) v(name)
  WHERE HAS_PERMS_BY_NAME(N'sys.partitions',N'OBJECT',N'SELECT',v.name,N'COLUMN') IS NULL) THEN 3
 ELSE 0 END;"#
    )
}
pub(crate) fn parameters(options: bool) -> String {
    fields(options)
        .map(|field| format!(",{} int", field.variable()))
        .chain(std::iter::once(",@culprit_cfg_partitions int".into()))
        .collect()
}
pub(crate) fn arguments(options: bool) -> String {
    fields(options)
        .map(|field| {
            let variable = field.variable();
            format!(",{variable}={variable}")
        })
        .chain(std::iter::once(",@culprit_cfg_partitions=@culprit_cfg_partitions".into()))
        .collect()
}

/// The existing SELECT evaluates a correlated catalog/JSON projection for each returned parent.
/// TOP bounds its returned prefix, not internal reads/sorts. At the 10,000-parent cap the projection can return 650,000
/// native metadata rows; ordinary use needs owned scale/cost evidence, not dispatch-count equivalence.
pub(crate) fn columns_expression(index: bool) -> String {
    let mut columns = Vec::new();
    if index {
        for field in OPTIONS {
            columns.push(field.value_expression("i", &format!("configuration_{}", field.name)));
            columns.push(field.availability_expression());
        }
    }
    let aliases =
        ["data_compression", "data_compression_description", "xml_compression", "xml_compression_description"];
    let values = COMPRESSION
        .into_iter()
        .zip(aliases)
        .map(|(field, alias)| field.value_expression("cp", alias))
        .collect::<Vec<_>>()
        .join("+N','+");
    let count = MAX_MSSQL_CONFIGURATION_PARTITIONS + 1;
    let index_id = if index { "i.index_id" } else { "0" };
    let prefix = crate::catalog_visibility::sql_literal(&format!(
        "(SELECT TOP({count}) cp.object_id,cp.index_id,CONVERT(varchar(20),cp.partition_id) AS partition_id,cp.partition_number,CONVERT(varchar(20),cp.hobt_id) AS hobt_id,"
    ));
    let suffix = crate::catalog_visibility::sql_literal(&format!(
        " FROM sys.partitions cp WHERE cp.object_id=t.object_id AND cp.index_id={index_id} ORDER BY cp.partition_number FOR JSON PATH,INCLUDE_NULL_VALUES)"
    ));
    columns.push(format!("(CASE WHEN @culprit_cfg_partitions=0 THEN CAST({prefix} AS nvarchar(max))+{values}+{suffix} ELSE N'CAST(NULL AS nvarchar(max))' END)+N' AS configuration_partitions'"));
    columns.push("N'@culprit_cfg_partitions AS configuration_partition_identity_availability'".into());
    columns.extend(COMPRESSION.into_iter().map(Field::availability_expression));
    columns.join("+N','+")
}

pub(crate) fn index(row: &Rec, index: &MssqlIndexSnapshot) -> Result<Box<MssqlIndexConfiguration>> {
    decode(
        row,
        NativeOwner {
            table: index.table.clone(),
            schema: index.schema.clone(),
            table_name: index.table_name.clone(),
            modified: index.table_modified_without_offset.clone(),
            index_id: index.index_id,
            name: Some(index.name.clone()),
            type_code: index.type_code,
        },
        Some(options(row)?),
    )
    .map(Box::new)
}
pub(crate) fn heap(row: &Rec, table: &MssqlTableSnapshot) -> Result<Option<Box<MssqlIndexConfiguration>>> {
    let configuration = decode(
        row,
        NativeOwner {
            table: table.identity.clone(),
            schema: table.schema.clone(),
            table_name: table.name.clone(),
            modified: table.modified_without_offset.clone(),
            index_id: 0,
            name: None,
            type_code: 0,
        },
        None,
    )?;
    if table.is_heap != Some(true) {
        return if configuration.partitions.is_empty() { Ok(None) } else { Err(invalid_data()) };
    }
    Ok(Some(Box::new(configuration)))
}
struct NativeOwner {
    table: MssqlObjectIdentity,
    schema: String,
    table_name: String,
    modified: String,
    index_id: u32,
    name: Option<String>,
    type_code: u32,
}
fn decode(row: &Rec, owner: NativeOwner, options: Option<MssqlStoredIndexOptions>) -> Result<MssqlIndexConfiguration> {
    let availability = |name| crate::catalog_visibility::availability(row, name);
    let partition_identity_availability = availability("configuration_partition_identity_availability")?;
    let partitions: Vec<MssqlPartitionCompression> = if let Some(raw) = optional_text(row, "configuration_partitions")?
    {
        if raw.len() > PARTITION_JSON_BYTES {
            return Err(CollectionStop::ByteLimit.error());
        }
        serde_json::from_str(&raw).map_err(|_| invalid_data())?
    } else if partition_identity_availability == MssqlCatalogAvailability::Available {
        return Err(invalid_data());
    } else {
        vec![]
    };
    if partitions.len() > MAX_MSSQL_CONFIGURATION_PARTITIONS + 1 {
        return Err(invalid_data());
    }
    let mut configuration = MssqlIndexConfiguration {
        table: owner.table,
        schema: owner.schema,
        table_name: owner.table_name,
        table_modified_without_offset: owner.modified,
        index_id: owner.index_id,
        name: owner.name,
        type_code: owner.type_code,
        options,
        partition_identity_availability,
        data_compression_availability: availability("configuration_data_compression_availability")?,
        data_compression_description_availability: availability("configuration_data_compression_desc_availability")?,
        xml_compression_availability: availability("configuration_xml_compression_availability")?,
        xml_compression_description_availability: availability("configuration_xml_compression_desc_availability")?,
        partitions_complete: partition_identity_availability == MssqlCatalogAvailability::Available
            && partitions.len() <= MAX_MSSQL_CONFIGURATION_PARTITIONS,
        partitions,
    };
    if !configuration.partition_rows_are_valid() {
        return Err(invalid_data());
    }
    configuration.partitions.truncate(MAX_MSSQL_CONFIGURATION_PARTITIONS);
    configuration.is_valid().then_some(configuration).ok_or_else(invalid_data)
}
fn options(row: &Rec) -> Result<MssqlStoredIndexOptions> {
    let availability =
        |name| crate::catalog_visibility::availability(row, &format!("configuration_{name}_availability"));
    let boolean = |name| optional_boolean(row, &format!("configuration_{name}"));
    Ok(MssqlStoredIndexOptions {
        fill_factor_availability: availability("fill_factor")?,
        fill_factor: optional_integer(row, "configuration_fill_factor")?
            .map(|value| u8::try_from(value).map_err(|_| invalid_data()))
            .transpose()?,
        padded_availability: availability("is_padded")?,
        padded: boolean("is_padded")?,
        ignore_duplicate_key_availability: availability("ignore_dup_key")?,
        ignore_duplicate_key: boolean("ignore_dup_key")?,
        allow_row_locks_availability: availability("allow_row_locks")?,
        allow_row_locks: boolean("allow_row_locks")?,
        allow_page_locks_availability: availability("allow_page_locks")?,
        allow_page_locks: boolean("allow_page_locks")?,
    })
}

#[cfg(test)]
mod tests;
