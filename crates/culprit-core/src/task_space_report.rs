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
    pub title: String,
    pub explanation: String,
    pub investigation: String,
    pub verify: String,
}
