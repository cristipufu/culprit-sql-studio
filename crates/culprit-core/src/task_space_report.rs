//! Explanations reference original task counters without treating current request context as allocation ownership.
use crate::{SourceState, telemetry::TaskSpaceIdentity};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct TaskSpaceReport {
    pub source_state: Option<SourceState>,
    pub source_reason: Option<String>,
    pub title: String,
    pub summary: String,
    pub limitations: Vec<String>,
    pub entries: Vec<TaskSpaceExplanation>,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct TaskSpaceExplanation {
    pub observation_key: String,
    pub task: TaskSpaceIdentity,
    pub activity: TaskAllocationActivity,
    pub title: String,
    pub explanation: String,
    pub investigation: String,
    pub verify: String,
}

/// Positive allocation or deallocation counters identify observed native object classes, not current footprint,
/// spilled operators or allocating statements. A single positive class never establishes the other class's absence.
#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum TaskAllocationActivity {
    UserObjects,
    InternalObjects,
    Mixed,
    /// Every native allocation/deallocation counter was measured as zero at this point, not throughout a period.
    NoRecordedActivity,
    /// No positive counter was recorded and at least one native counter was unavailable.
    Unknown,
}
