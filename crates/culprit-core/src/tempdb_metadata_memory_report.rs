//! Native per-consumer reservation/usage points preserve unavailable values without a pressure or owner verdict.
use crate::{
    SourceState,
    telemetry::{ObservationReadContext, StateSourceCoverage},
};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct TempdbMetadataMemoryEntry {
    pub observation_key: String,
    pub observed_at: String,
    pub native_consumer_id: String,
    pub native_type_code: String,
    pub native_type_name: String,
    #[serde(deserialize_with = "Option::deserialize")]
    pub description: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub allocated_bytes: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub used_bytes: Option<String>,
    pub explanation: String,
    pub investigation: String,
    pub verify: String,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct TempdbMetadataMemoryReport {
    pub context: ObservationReadContext,
    pub native_source: StateSourceCoverage,
    pub source_state: SourceState,
    pub read_complete: bool,
    #[serde(deserialize_with = "Option::deserialize")]
    pub metadata_enabled: Option<bool>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub feature_observed_at: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub tempdb_created_without_offset: Option<String>,
    /// Source rows include the independent native feature point; they are not consumer totals.
    pub native_rows: u32,
    pub observed_rows: u32,
    pub title: String,
    pub summary: String,
    pub limitations: Vec<String>,
    pub entries: Vec<TempdbMetadataMemoryEntry>,
}
