//! An informational interpretation of original CDC points, never a unique event feed or freshness verdict.
use crate::{SourceState, telemetry::*};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum CdcEntryKind {
    ScanSession,
    Aggregate,
    Error,
}
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct CdcField {
    pub key: String,
    pub label: String,
    pub value: Option<String>,
}
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct CdcEntry {
    pub source: String,
    pub observation_key: String,
    pub kind: CdcEntryKind,
    pub observed_at: String,
    pub title: String,
    pub explanation: String,
    pub fields: Vec<CdcField>,
    pub investigation: String,
    pub verify: String,
}
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct CdcReport {
    pub context: NativeReadContext,
    pub sources: Vec<StateSourceCoverage>,
    pub source_state: SourceState,
    pub read_complete: bool,
    pub enabled: Option<bool>,
    pub title: String,
    pub summary: String,
    pub limitations: Vec<String>,
    pub entries: Vec<CdcEntry>,
}
