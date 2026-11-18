//! Displayed configured limits preserve their native scopes and exact integer values.
use crate::{Metric, SourceState, telemetry::NativeReadContext};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum ConfiguredLogRateScope {
    UserWorkloadGroup,
    UserResourcePool,
    DatabaseEngineInstance,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug)]
#[ts(export)]
pub struct ConfiguredLogRateEntry {
    pub scope: ConfiguredLogRateScope,
    pub bytes_per_second: Option<String>,
    pub title: String,
    pub explanation: String,
    pub metric: Metric,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug)]
#[ts(export)]
pub struct ConfiguredLogRateReport {
    pub context: NativeReadContext,
    pub source_state: SourceState,
    pub source_reason: Option<String>,
    pub read_complete: bool,
    pub native_rows: u32,
    pub observed_rows: u32,
    pub observed_at: Option<String>,
    pub workload_group_id: Option<String>,
    pub title: String,
    pub summary: String,
    pub limitations: Vec<String>,
    pub entries: Vec<ConfiguredLogRateEntry>,
}
