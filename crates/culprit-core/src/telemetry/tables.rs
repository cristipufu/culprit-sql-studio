//! Native table metadata and approximate partition counters, without reading application rows.
use super::*;

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum MssqlTableSizeSource {
    PartitionStatistics,
    CatalogAllocations,
    Unavailable,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct MssqlTableSnapshot {
    pub identity: MssqlObjectIdentity,
    pub schema: String,
    pub name: String,
    pub modified_without_offset: String,
    /// SQL Server's approximate row count, not COUNT(*) and not a cumulative counter.
    pub estimated_rows: Option<String>,
    pub reserved_bytes: Option<String>,
    pub data_bytes: Option<String>,
    pub index_bytes: Option<String>,
    pub in_row_bytes: Option<String>,
    pub lob_bytes: Option<String>,
    pub row_overflow_bytes: Option<String>,
    pub has_primary_key: Option<bool>,
    pub is_heap: Option<bool>,
    pub size_source: MssqlTableSizeSource,
}
impl MssqlTableSnapshot {
    pub fn is_valid(&self) -> bool {
        let sizes = [
            &self.reserved_bytes,
            &self.data_bytes,
            &self.index_bytes,
            &self.in_row_bytes,
            &self.lob_bytes,
            &self.row_overflow_bytes,
        ];
        self.identity.is_valid()
            && [&self.schema, &self.name].into_iter().all(|name| !name.is_empty() && name.len() <= 2048)
            && super::catalog::local_time(&self.modified_without_offset)
            && self.estimated_rows.as_deref().is_none_or(valid_counter)
            && sizes.iter().all(|value| value.as_deref().is_none_or(valid_counter))
            && (self.size_source != MssqlTableSizeSource::Unavailable || sizes.iter().all(|value| value.is_none()))
    }
}
