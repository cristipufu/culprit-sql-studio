//! A native statement counter point retains its exact identity and reset qualification without copying raw rows.
use crate::{ResetMarker, SourceState, TemporaryWorkExplanation, telemetry::NativeQueryIdentity};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Serialize, Deserialize, TS, Clone, Debug)]
#[ts(export)]
pub struct StatementWorkReport {
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
    pub observation_key: String,
    pub identity: NativeQueryIdentity,
    pub observed_at: String,
    pub reset: ResetMarker,
    pub work: Vec<TemporaryWorkExplanation>,
}
