//! Persisted current configuration only: ordinals do not measure physical order or rowgroup health.
use super::*;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct MssqlColumnstoreColumn {
    pub index_column_id: u32,
    pub column_id: u32,
    pub name: String,
    #[serde(deserialize_with = "required_nullable")]
    pub column_store_order_ordinal: Option<u8>,
    #[serde(deserialize_with = "required_nullable")]
    pub data_clustering_ordinal: Option<u8>,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct MssqlColumnstoreConfiguration {
    /// Database-local native owner, not a global database or index incarnation.
    pub table: MssqlObjectIdentity,
    pub index_id: u32,
    pub schema: String,
    pub table_name: String,
    pub name: String,
    pub table_modified_without_offset: String,
    pub compression_delay_availability: MssqlCatalogAvailability,
    /// Configured minutes. Available + NULL is native automatic management, distinct from an unread field.
    #[serde(deserialize_with = "required_nullable")]
    pub compression_delay_minutes: Option<u32>,
    pub column_store_order_availability: MssqlCatalogAvailability,
    pub data_clustering_availability: MssqlCatalogAvailability,
    pub columns: Vec<MssqlColumnstoreColumn>,
}
impl MssqlColumnstoreConfiguration {
    pub fn is_valid(&self) -> bool {
        let mut positions = BTreeSet::new();
        let mut columns = BTreeSet::new();
        let mut order = BTreeSet::new();
        let mut clustering = BTreeSet::new();
        self.table.is_valid()
            && self.index_id > 0
            && self.index_id <= i32::MAX as u32
            && [&self.schema, &self.table_name, &self.name]
                .into_iter()
                .all(|name| !name.is_empty() && name.len() <= 2048)
            && super::super::catalog::local_time(&self.table_modified_without_offset)
            && (self.compression_delay_availability == MssqlCatalogAvailability::Available
                || self.compression_delay_minutes.is_none())
            && self.compression_delay_minutes.is_none_or(|minutes| minutes <= i32::MAX as u32)
            && self.columns.len() <= MAX_MSSQL_INDEX_COLUMNS
            && self.columns.iter().all(|column| {
                column.index_column_id > 0
                    && column.index_column_id <= i32::MAX as u32
                    && column.column_id > 0
                    && column.column_id <= i32::MAX as u32
                    && !column.name.is_empty()
                    && column.name.len() <= 2048
                    && positions.insert(column.index_column_id)
                    && columns.insert(column.column_id)
                    && valid_ordinal(
                        column.column_store_order_ordinal,
                        self.column_store_order_availability,
                        &mut order,
                    )
                    && valid_ordinal(column.data_clustering_ordinal, self.data_clustering_availability, &mut clustering)
            })
    }

    /// Negative consistency checks within one snapshot; names do not authenticate an independent source.
    pub fn is_for_index(&self, index: &crate::IndexStat) -> bool {
        self.is_valid() && self.schema == index.schema && self.table_name == index.table && self.name == index.name
    }

    pub(super) fn is_valid_for(&self, index: &MssqlIndexSnapshot) -> bool {
        if !self.is_valid()
            || !matches!(index.type_code, 5 | 6)
            || self.table != index.table
            || self.index_id != index.index_id
            || self.schema != index.schema
            || self.table_name != index.table_name
            || self.name != index.name
            || self.table_modified_without_offset != index.table_modified_without_offset
            || self.columns.len() != index.columns.len()
        {
            return false;
        }
        let columns = index.columns.iter().map(|column| (column.index_column_id, column)).collect::<BTreeMap<_, _>>();
        self.columns.iter().all(|column| {
            columns
                .get(&column.index_column_id)
                .is_some_and(|original| original.column_id == column.column_id && original.name == column.name)
        })
    }
}

fn required_nullable<'de, D: serde::Deserializer<'de>, T: Deserialize<'de>>(
    reader: D,
) -> std::result::Result<Option<T>, D::Error> {
    Option::<T>::deserialize(reader)
}

fn valid_ordinal(value: Option<u8>, availability: MssqlCatalogAvailability, used: &mut BTreeSet<u8>) -> bool {
    match availability {
        MssqlCatalogAvailability::Available => value.is_none_or(|ordinal| ordinal == 0 || used.insert(ordinal)),
        _ => value.is_none(),
    }
}

#[cfg(test)]
mod tests;
