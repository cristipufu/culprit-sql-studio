//! A native statement counter point retains its exact identity and reset qualification without copying raw rows.
use crate::{MeasurementScope, ResetMarker, SourceState, TemporaryWorkExplanation, telemetry::NativeQueryIdentity};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Serialize, Deserialize, TS, Clone, Debug, Default, PartialEq, Eq)]
#[ts(export)]
pub struct StatementOutcomeCounts {
    pub errors: Option<String>,
    pub warnings: Option<String>,
}

impl StatementOutcomeCounts {
    pub fn is_recorded(&self) -> bool {
        self.errors.is_some() || self.warnings.is_some()
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Debug)]
#[ts(export)]
pub struct StatementWorkReport {
    pub source: String,
    pub context: Option<Box<crate::telemetry::NativeReadContext>>,
    pub source_state: Option<SourceState>,
    pub source_reason: Option<String>,
    pub title: String,
    pub summary: String,
    pub limitations: Vec<String>,
    pub entries: Vec<StatementWorkEntry>,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug)]
#[ts(export)]
pub struct StatementWorkEntry {
    pub source: String,
    pub observation_key: String,
    pub identity: NativeQueryIdentity,
    /// The population declared by this native point, independent from the member that read it.
    pub scope: MeasurementScope,
    pub observed_at: String,
    pub reset: ResetMarker,
    pub work: Vec<TemporaryWorkExplanation>,
    pub outcomes: StatementOutcomeCounts,
    pub outcome_metrics: Vec<crate::Metric>,
}
