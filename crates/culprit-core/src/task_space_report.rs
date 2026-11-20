//! Explanations reference original task counters without treating current request context as allocation ownership.
use crate::{
    SourceState,
    telemetry::{ActivityClient, ActivityKey, DefinitionPayload, TaskSpaceIdentity},
};
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
    /// Qualified current request sampled between its native checks, never the allocating statement.
    #[serde(deserialize_with = "Option::deserialize")]
    pub current_request: Option<TaskSpaceCurrentRequest>,
    pub title: String,
    pub explanation: String,
    pub investigation: String,
    pub verify: String,
}

/// Current-at-capture request context. Its independent check interval does not date the task counters,
/// establish task continuity, identify the allocating statement or assign a module or temporary-space cause.
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct TaskSpaceCurrentRequest {
    pub request: ActivityKey,
    pub observed_from: String,
    pub observed_until: String,
    pub statement: DefinitionPayload,
    #[serde(deserialize_with = "Option::deserialize")]
    pub client: Option<ActivityClient>,
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
