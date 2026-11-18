//! Exact source wait counters remain independent from categories, sampled deltas and statement time.
use crate::{Metric, SourceOutcome, telemetry::*};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum WaitCounterIssue {
    SignalExceedsTotal,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug)]
#[ts(export)]
pub struct WaitCounterEntry {
    pub observation: WaitCounterObservation,
    /// Checked total minus signal; unavailable when original native values contradict that subtraction.
    pub resource_wait_time_ms: Option<String>,
    pub issues: Vec<WaitCounterIssue>,
    pub title: String,
    pub explanation: String,
    pub metrics: Vec<Metric>,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug)]
#[ts(export)]
pub struct WaitCounterReport {
    pub context: Option<ObservationReadContext>,
    pub sources: Vec<SourceOutcome>,
    pub read_complete: bool,
    pub native_rows: u32,
    pub observed_rows: u32,
    pub presented_rows: u32,
    pub entries_limited: bool,
    pub title: String,
    pub summary: String,
    pub limitations: Vec<String>,
    pub entries: Vec<WaitCounterEntry>,
}
