use super::fields::{native_big_id, signed_integer};
use crate::{
    read::{integer, invalid_data, optional_integer, optional_text, text},
    value::Rec,
};
use culprit_core::Result;
use culprit_core::telemetry::canonical_time;

/// A local native address, never a global page identity or a claim that a waiting request owns the latch.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct PageAddress {
    pub database: u32,
    pub file: u32,
    pub page: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct HeaderRecord {
    pub ordinal: usize,
    pub address: PageAddress,
    pub observed_at: String,
    pub header: super::fields::Header,
    pub catalog: Catalog,
}
impl HeaderRecord {
    pub(super) fn decode(row: &Rec) -> Result<Self> {
        Ok(Self {
            ordinal: usize::try_from(integer(row, "ordinal")?).map_err(|_| invalid_data())?,
            address: PageAddress {
                database: u32::try_from(integer(row, "target_database_id")?).map_err(|_| invalid_data())?,
                file: u32::try_from(integer(row, "target_file_id")?).map_err(|_| invalid_data())?,
                page: u32::try_from(integer(row, "target_page_id")?).map_err(|_| invalid_data())?,
            },
            observed_at: canonical_time(&text(row, "observed_at")?)?,
            header: super::fields::Header::decode(row)?,
            catalog: Catalog::decode(row)?,
        })
    }

    /// Equal endpoint observations are only a bounded mapping guard. They never prove continuous allocation,
    /// eliminate ABA reuse, identify a latch holder, or turn page-content changes into a different resource owner.
    pub(super) fn same_mapping(&self, other: &Self) -> bool {
        self.address == other.address
            && self.catalog.file_guid.is_some()
            && self.catalog.file_guid == other.catalog.file_guid
            && self.catalog == other.catalog
            && self.header.database_id == other.header.database_id
            && self.header.file_id == other.header.file_id
            && self.header.page_id == other.header.page_id
            && self.header.header_version == other.header.header_version
            && self.header.page_type == other.header.page_type
            && self.header.level == other.header.level
            && self.header.object_id == other.header.object_id
            && self.header.index_id == other.header.index_id
            && self.header.partition_id == other.header.partition_id
            && self.header.allocation_unit_id == other.header.allocation_unit_id
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Catalog {
    pub file_guid: Option<String>,
    pub file_create_lsn: Option<String>,
    pub file_type: Option<i32>,
    pub file_state: Option<i32>,
    pub object_id: Option<i32>,
    pub schema_name: Option<String>,
    pub object_name: Option<String>,
    pub object_created: Option<String>,
    pub object_modified: Option<String>,
    pub index_id: Option<i32>,
    pub index_name: Option<String>,
    pub index_type: Option<i32>,
    pub partition_id: Option<String>,
    pub partition_number: Option<u32>,
    pub hobt_id: Option<String>,
}
impl Catalog {
    fn decode(row: &Rec) -> Result<Self> {
        Ok(Self {
            file_guid: optional_text(row, "file_guid")?,
            file_create_lsn: optional_text(row, "file_create_lsn")?,
            file_type: signed_integer(row, "file_type")?,
            file_state: signed_integer(row, "file_state")?,
            object_id: signed_integer(row, "catalog_object_id")?,
            schema_name: optional_text(row, "schema_name")?,
            object_name: optional_text(row, "object_name")?,
            object_created: native_time(row, "object_created_without_offset")?,
            object_modified: native_time(row, "object_modified_without_offset")?,
            index_id: signed_integer(row, "catalog_index_id")?,
            index_name: optional_text(row, "index_name")?,
            index_type: signed_integer(row, "index_type")?,
            partition_id: native_big_id(row, "catalog_partition_id")?,
            partition_number: optional_integer(row, "catalog_partition_number")?
                .map(|number| number.try_into().map_err(|_| invalid_data()))
                .transpose()?,
            hobt_id: native_big_id(row, "catalog_hobt_id")?,
        })
    }
}
fn native_time(row: &Rec, field: &str) -> Result<Option<String>> {
    optional_text(row, field)?.map(|value| crate::sql_time::native_time(&value)).transpose()
}
impl PageAddress {
    pub(super) fn parse(native: &str) -> Option<Self> {
        let mut parts = native.split(':');
        let database = positive(parts.next()?, i16::MAX as u32)?;
        let file = positive(parts.next()?, i32::MAX as u32)?;
        let page = decimal(parts.next()?, i32::MAX as u32)?;
        if parts.next().is_some() {
            return None;
        }
        Some(Self { database, file, page })
    }

    pub(super) fn matches(&self, header: &super::fields::Header) -> Result<bool> {
        match (header.database_id, header.file_id, header.page_id) {
            (None, None, None) => Ok(false),
            (Some(database), Some(file), Some(page))
                if database == self.database && file == self.file && page == self.page =>
            {
                Ok(true)
            }
            _ => Err(invalid_data()),
        }
    }
}
fn positive(value: &str, max: u32) -> Option<u32> {
    decimal(value, max).filter(|value| *value > 0)
}
fn decimal(value: &str, max: u32) -> Option<u32> {
    if value.is_empty() || value.len() > 10 || !value.bytes().all(|value| value.is_ascii_digit()) {
        return None;
    }
    value.parse::<u32>().ok().filter(|value| *value <= max)
}
