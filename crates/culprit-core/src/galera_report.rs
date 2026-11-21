//! One observed MariaDB Galera node; no query, database or global-cluster attribution.
use serde::{Deserialize, Serialize};
use ts_rs::TS;
#[derive(Serialize, Deserialize, TS, Clone, Debug)]
#[ts(export)]
pub struct GaleraReport {
    pub original: Option<GaleraOrigin>,
    pub sources: Vec<crate::SourceOutcome>,
    pub read_complete: bool,
    pub native_rows: Option<u32>,
    pub observed_rows: u32,
    pub presented_rows: u32,
    pub entries_limited: bool,
    pub title: String,
    pub summary: String,
    pub limitations: Vec<String>,
    pub entries: Vec<crate::ReplicationEntry>,
}
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct GaleraOrigin {
    pub context: crate::telemetry::NativeReadContext,
    pub metadata: crate::telemetry::MariadbGaleraMetadata,
}
impl GaleraReport {
    pub fn public_fields(entry: &crate::ReplicationEntry) -> impl Iterator<Item = &crate::ReplicationField> {
        entry.fields.iter().filter(|field| {
            !matches!(field.key.as_str(), "wsrep_cluster_state_uuid" | "wsrep_local_state_uuid" | "wsrep_gcomm_uuid")
        })
    }
}
