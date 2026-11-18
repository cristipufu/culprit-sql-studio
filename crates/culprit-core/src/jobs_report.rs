//! Job records explain native scheduler state without assigning a worker, SQL execution or incident cause.
use crate::{Metric, SourceOutcome, telemetry::NativeReadContext};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum JobEntryKind {
    State,
    RetainedRun,
    AgentContext,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug)]
#[ts(export)]
pub struct JobEntry {
    pub source: String,
    pub observation_key: String,
    pub job_id: Option<String>,
    pub kind: JobEntryKind,
    pub title: String,
    pub explanation: String,
    pub metrics: Vec<Metric>,
    pub investigation: String,
    pub verify: String,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug)]
#[ts(export)]
pub struct JobsReport {
    pub context: NativeReadContext,
    pub sources: Vec<SourceOutcome>,
    pub read_complete: bool,
    pub native_rows: u32,
    pub observed_rows: u32,
    pub entries_limited: bool,
    pub title: String,
    pub summary: String,
    pub limitations: Vec<String>,
    pub entries: Vec<JobEntry>,
}
