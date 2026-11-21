//! An informational interpretation of original CDC points, never a unique event feed or freshness verdict.
use crate::{SourceState, telemetry::*};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct CdcReport {
    pub context: Option<NativeReadContext>,
    pub sources: Vec<StateSourceCoverage>,
    pub source_state: SourceState,
    pub read_complete: bool,
    pub enabled: Option<bool>,
    pub title: String,
    pub summary: String,
    pub limitations: Vec<String>,
    pub entries: Vec<crate::ChangeCaptureEntry>,
}
