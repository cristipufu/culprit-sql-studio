//! Shared prose keeps each native component and its independent report clock visible.
use crate::{Metric, SourceState, telemetry::*};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Serialize, Deserialize, TS, Clone, Debug)]
#[ts(export)]
pub struct HyperscaleLogComponentEntry {
    pub observation_key: String,
    pub component_id: String,
    pub replica_id: Option<String>,
    pub file_id: Option<String>,
    pub current_max_log_rate_bps: String,
    pub catchup_rate_bps: String,
    pub catchup_distance_bytes: String,
    pub role: u8,
    pub role_desc: Option<String>,
    pub last_reported_time_without_offset: Option<String>,
    pub title: String,
    pub explanation: String,
    pub metrics: Vec<Metric>,
}
#[derive(Serialize, Deserialize, TS, Clone, Debug)]
#[ts(export)]
pub struct HyperscaleLogComponentReport {
    pub context: NativeReadContext,
    pub native_source: StateSourceCoverage,
    pub source_state: SourceState,
    pub source_reason: Option<String>,
    pub read_complete: bool,
    pub native_rows: u32,
    pub observed_rows: u32,
    pub presented_rows: u32,
    pub entries_limited: bool,
    pub title: String,
    pub summary: String,
    pub limitations: Vec<String>,
    pub entries: Vec<HyperscaleLogComponentEntry>,
}
