//! Persisted index/heap configuration. Native partitions remain individual rows, never a whole-table setting.
use super::*;
use std::collections::BTreeSet;

pub const MAX_MSSQL_CONFIGURATION_PARTITIONS: usize = 64;

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct MssqlStoredIndexOptions {
    pub fill_factor_availability: MssqlCatalogAvailability,
    /// Native zero means the creation/rebuild default, not measured free space or current density.
    #[serde(deserialize_with = "required_nullable")]
    pub fill_factor: Option<u8>,
    pub padded_availability: MssqlCatalogAvailability,
    #[serde(deserialize_with = "required_nullable")]
    pub padded: Option<bool>,
    pub ignore_duplicate_key_availability: MssqlCatalogAvailability,
    #[serde(deserialize_with = "required_nullable")]
    pub ignore_duplicate_key: Option<bool>,
    pub allow_row_locks_availability: MssqlCatalogAvailability,
    #[serde(deserialize_with = "required_nullable")]
    pub allow_row_locks: Option<bool>,
    pub allow_page_locks_availability: MssqlCatalogAvailability,
    #[serde(deserialize_with = "required_nullable")]
    pub allow_page_locks: Option<bool>,
}
impl MssqlStoredIndexOptions {
    pub fn is_valid(&self) -> bool {
        supplied(self.fill_factor_availability, &self.fill_factor)
            && self.fill_factor.is_none_or(|value| value <= 100)
            && supplied(self.padded_availability, &self.padded)
            && supplied(self.ignore_duplicate_key_availability, &self.ignore_duplicate_key)
            && supplied(self.allow_row_locks_availability, &self.allow_row_locks)
            && supplied(self.allow_page_locks_availability, &self.allow_page_locks)
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct MssqlPartitionCompression {
    /// Exact native bigint identities, not JavaScript numbers or database-incarnation proof.
    pub object_id: u32,
    pub index_id: u32,
    pub partition_id: String,
    pub partition_number: u32,
    /// Native zero is retained; it is not invented into a HoBT identity for an unsupported index kind.
    pub hobt_id: String,
    #[serde(deserialize_with = "required_nullable")]
    pub data_compression: Option<u8>,
    #[serde(deserialize_with = "required_nullable")]
    pub data_compression_description: Option<String>,
    #[serde(deserialize_with = "required_nullable")]
    pub xml_compression: Option<bool>,
    #[serde(deserialize_with = "required_nullable")]
    pub xml_compression_description: Option<String>,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct MssqlIndexConfiguration {
    pub table: MssqlObjectIdentity,
    pub schema: String,
    pub table_name: String,
    pub table_modified_without_offset: String,
    /// Zero is the native heap, not a named index. This does not supply an independent index incarnation.
    pub index_id: u32,
    #[serde(deserialize_with = "required_nullable")]
    pub name: Option<String>,
    pub type_code: u32,
    #[serde(deserialize_with = "required_nullable")]
    pub options: Option<MssqlStoredIndexOptions>,
    pub partition_identity_availability: MssqlCatalogAvailability,
    pub data_compression_availability: MssqlCatalogAvailability,
    pub data_compression_description_availability: MssqlCatalogAvailability,
    pub xml_compression_availability: MssqlCatalogAvailability,
    pub xml_compression_description_availability: MssqlCatalogAvailability,
    /// False also covers a bounded prefix, absent permission or unsupported/unknown native partition layout.
    pub partitions_complete: bool,
    pub partitions: Vec<MssqlPartitionCompression>,
}
impl MssqlIndexConfiguration {
    pub fn is_valid(&self) -> bool {
        self.table.is_valid()
            && self.table.database_id <= i32::MAX as u32
            && self.table.object_id <= i32::MAX as u32
            && self.index_id <= i32::MAX as u32
            && self.type_code <= u8::MAX as u32
            && [&self.schema, &self.table_name].into_iter().all(|name| label(name))
            && super::super::catalog::local_time(&self.table_modified_without_offset)
            && if self.index_id == 0 {
                self.type_code == 0 && self.name.is_none() && self.options.is_none()
            } else {
                self.type_code > 0
                    && self.name.as_deref().is_some_and(label)
                    && self.options.as_ref().is_some_and(MssqlStoredIndexOptions::is_valid)
            }
            && self.partitions.len() <= MAX_MSSQL_CONFIGURATION_PARTITIONS
            && (self.partition_identity_availability == MssqlCatalogAvailability::Available
                || self.partitions.is_empty() && !self.partitions_complete)
            && self.partition_rows_are_valid()
    }
    /// Validate all native rows before a producer retains its bounded prefix, including its one lookahead.
    pub fn partition_rows_are_valid(&self) -> bool {
        let mut identities = BTreeSet::new();
        let mut numbers = BTreeSet::new();
        self.partitions.iter().all(|partition| {
            partition.object_id == self.table.object_id
                && partition.index_id == self.index_id
                && integer_id(&partition.partition_id, false)
                && integer_id(&partition.hobt_id, true)
                && partition.partition_number > 0
                && identities.insert(&partition.partition_id)
                && numbers.insert(partition.partition_number)
                && supplied(self.data_compression_availability, &partition.data_compression)
                && supplied(self.data_compression_description_availability, &partition.data_compression_description)
                && supplied(self.xml_compression_availability, &partition.xml_compression)
                && supplied(self.xml_compression_description_availability, &partition.xml_compression_description)
                && description(partition.data_compression, partition.data_compression_description.as_deref())
                && match (&partition.xml_compression, partition.xml_compression_description.as_deref()) {
                    (Some(true), Some("OFF")) | (Some(false), Some("ON")) => false,
                    (_, None | Some("ON" | "OFF")) => true,
                    (_, Some(value)) => !value.is_empty() && value.len() <= 60,
                }
        }) && self.partitions.windows(2).all(|rows| rows[0].partition_number < rows[1].partition_number)
    }
    pub fn is_for_index(&self, index: &crate::IndexStat) -> bool {
        self.is_valid()
            && self.index_id > 0
            && self.schema == index.schema
            && self.table_name == index.table
            && self.name.as_deref() == Some(index.name.as_str())
    }
    pub fn is_for_table(&self, table: &crate::TableStat) -> bool {
        self.is_valid()
            && self.index_id == 0
            && table.is_heap == Some(true)
            && self.schema == table.schema
            && self.table_name == table.name
    }
    pub(super) fn is_valid_for(&self, index: &MssqlIndexSnapshot) -> bool {
        self.is_valid()
            && self.table == index.table
            && self.index_id == index.index_id
            && self.name.as_deref() == Some(index.name.as_str())
            && self.type_code == index.type_code
            && self.schema == index.schema
            && self.table_name == index.table_name
            && self.table_modified_without_offset == index.table_modified_without_offset
    }
    pub(crate) fn is_valid_for_heap(&self, table: &MssqlTableSnapshot) -> bool {
        self.is_valid()
            && self.index_id == 0
            && self.type_code == 0
            && table.is_heap == Some(true)
            && self.table == table.identity
            && self.schema == table.schema
            && self.table_name == table.name
            && self.table_modified_without_offset == table.modified_without_offset
    }
}
fn description(code: Option<u8>, value: Option<&str>) -> bool {
    let known = [(0, "NONE"), (1, "ROW"), (2, "PAGE"), (3, "COLUMNSTORE"), (4, "COLUMNSTORE_ARCHIVE")];
    value.is_none_or(|value| {
        !value.is_empty()
            && value.len() <= 60
            && known
                .iter()
                .find(|(_, text)| *text == value)
                .is_none_or(|(native, _)| code.is_none_or(|code| code == *native))
    })
}
fn supplied<T>(availability: MssqlCatalogAvailability, value: &Option<T>) -> bool {
    availability == MssqlCatalogAvailability::Available || value.is_none()
}
fn integer_id(value: &str, zero_allowed: bool) -> bool {
    value
        .parse::<u64>()
        .ok()
        .is_some_and(|number| number <= i64::MAX as u64 && (zero_allowed || number > 0) && number.to_string() == value)
}
fn label(value: &str) -> bool {
    !value.is_empty() && value.len() <= 2048
}
pub(super) fn required_nullable<'de, D: serde::Deserializer<'de>, T: Deserialize<'de>>(
    reader: D,
) -> std::result::Result<Option<T>, D::Error> {
    Option::<T>::deserialize(reader)
}

#[cfg(test)]
mod tests;
