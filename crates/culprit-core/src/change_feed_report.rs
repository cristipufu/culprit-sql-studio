//! Informational original change-feed history, never an event count, delivery guarantee or current lag verdict.
use crate::{ChangeCaptureEntry, SourceState, telemetry::*};
use serde::{Deserialize, Serialize};
use ts_rs::TS;
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct ChangeFeedReport {
    pub context: Option<NativeReadContext>,
    pub sources: Vec<StateSourceCoverage>,
    pub source_state: SourceState,
    pub read_complete: bool,
    pub title: String,
    pub summary: String,
    pub limitations: Vec<String>,
    pub entries: Vec<ChangeCaptureEntry>,
}
