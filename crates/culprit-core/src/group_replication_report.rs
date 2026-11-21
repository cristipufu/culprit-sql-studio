//! An observed member view and native counters are historical context, not database/query attribution.
use serde::{Deserialize, Serialize};
use ts_rs::TS;
#[derive(Serialize, Deserialize, TS, Clone, Debug)]
#[ts(export)]
pub struct GroupReplicationReport {
    pub original: Option<GroupReplicationOrigin>,
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
pub struct GroupReplicationOrigin {
    pub context: crate::telemetry::NativeReadContext,
    pub metadata: crate::telemetry::MysqlGroupReplicationMetadata,
}

impl GroupReplicationReport {
    /// Curated public consumers share one closed field projection; native member/view IDs stay original-owned.
    pub fn public_fields(entry: &crate::ReplicationEntry) -> impl Iterator<Item = &crate::ReplicationField> {
        entry.fields.iter().filter(|field| {
            matches!(field.key.as_str(), "state" | "role" | "version" | "member_observed_at" | "statistics_observed_at")
        })
    }
}
