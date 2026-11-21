//! A bounded catalog observation, never base-table maintenance or an exclusive workload contribution.
use crate::{
    IndexConfigurationEntry, SourceOutcome,
    telemetry::{MssqlCatalogIndexColumn, MssqlIndexUsage, MssqlObjectIdentity, NativeReadContext},
};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Serialize, Deserialize, TS, Clone, Debug)]
#[ts(export)]
pub struct IndexedViewIndexEntry {
    pub observation_key: Option<String>,
    pub parent_object: MssqlObjectIdentity,
    pub parent_schema: String,
    pub parent_name: String,
    pub parent_modified_without_offset: String,
    pub index_id: u32,
    pub index_name: String,
    pub type_code: u32,
    pub native_type: String,
    pub unique: bool,
    pub disabled: bool,
    pub reserved_bytes: Option<String>,
    pub native_columns: u32,
    pub columns_limited: bool,
    pub columns: Vec<MssqlCatalogIndexColumn>,
    pub usage: MssqlIndexUsage,
    pub configuration: Option<Box<IndexConfigurationEntry>>,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug)]
#[ts(export)]
pub struct IndexedViewsReport {
    /// A current scan has no authenticated original context; it cannot supply one by matching names.
    pub context: Option<NativeReadContext>,
    pub source: SourceOutcome,
    pub read_complete: Option<bool>,
    /// Combined native Indexes rows, including table indexes. Unknown after refused acquisition.
    pub native_index_rows: Option<u32>,
    pub observed_index_rows: u32,
    pub observed_view_indexes: u32,
    pub entries_limited: bool,
    pub title: String,
    pub summary: String,
    pub qualification: String,
    pub entries: Vec<IndexedViewIndexEntry>,
    pub limitations: Vec<String>,
}
