//! Shared aggregate fractions retain their exact native inputs and explicit limitations.
use crate::{
    MeasurementScope, Metric, SourceState,
    telemetry::{CounterRatioIssue, CounterRatioKind},
};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Serialize, Deserialize, TS, Clone, Debug)]
#[ts(export)]
pub struct CounterRatioEntry {
    pub observation_key: String,
    pub kind: CounterRatioKind,
    pub scope: MeasurementScope,
    pub observed_at: Option<String>,
    /// Absent when neither expected native input was observed; the source expectation is not a native object.
    pub native_object: Option<String>,
    pub native_instance: Option<String>,
    pub issues: Vec<CounterRatioIssue>,
    pub title: String,
    pub explanation: String,
    pub metrics: Vec<Metric>,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug)]
#[ts(export)]
pub struct CounterRatioReport {
    pub source_state: Option<SourceState>,
    pub source_reason: Option<String>,
    pub read_complete: bool,
    pub declared_ratios: u32,
    pub presented_ratios: u32,
    pub entries_limited: bool,
    pub title: String,
    pub summary: String,
    pub limitations: Vec<String>,
    pub entries: Vec<CounterRatioEntry>,
}
