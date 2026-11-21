//! Native persisted configuration is informational and keeps every partition as its own observed row.
use crate::{
    SourceState,
    telemetry::{
        MssqlCatalogAvailability, MssqlIndexParentKind, MssqlObjectIdentity, NativeReadContext, SnapshotDataset,
    },
};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Serialize, Deserialize, TS, Clone, Debug)]
#[ts(export)]
pub struct IndexConfigurationField {
    pub key: String,
    pub label: String,
    pub text: String,
    pub availability: MssqlCatalogAvailability,
}
#[derive(Serialize, Deserialize, TS, Clone, Debug)]
#[ts(export)]
pub struct PartitionConfigurationEntry {
    pub partition_id: String,
    pub partition_number: u32,
    pub hobt_id: String,
    pub fields: Vec<IndexConfigurationField>,
}
#[derive(Serialize, Deserialize, TS, Clone, Debug)]
#[ts(export)]
pub struct IndexConfigurationEntry {
    pub observation_key: Option<String>,
    pub parent_kind: MssqlIndexParentKind,
    pub parent_object: MssqlObjectIdentity,
    pub parent_schema: String,
    pub parent_name: String,
    pub parent_modified_without_offset: String,
    pub index_id: u32,
    pub index_name: Option<String>,
    pub type_code: u32,
    pub fields: Vec<IndexConfigurationField>,
    pub partition_identity_availability: MssqlCatalogAvailability,
    pub partitions_complete: bool,
    pub retained_partitions: u32,
    pub presented_partitions: u32,
    pub partitions_limited: bool,
    pub partitions: Vec<PartitionConfigurationEntry>,
    pub limitations: Vec<String>,
}
#[derive(Serialize, Deserialize, TS, Clone, Debug)]
#[ts(export)]
pub struct IndexConfigurationReport {
    /// Unknown for an explicit withdrawn original; a current scan never supplies its missing scope.
    pub dataset: Option<SnapshotDataset>,
    /// Authenticated originals retain this context independently of the current scan. Ordinary scan fields have none.
    pub context: Option<NativeReadContext>,
    pub title: String,
    pub summary: String,
    pub qualification: String,
    pub source_state: Option<SourceState>,
    pub source_reason: Option<String>,
    pub read_complete: Option<bool>,
    pub native_rows: Option<u32>,
    pub observed_rows: u32,
    pub presented_rows: u32,
    pub entries_limited: bool,
    pub entries: Vec<IndexConfigurationEntry>,
    pub limitations: Vec<String>,
}
