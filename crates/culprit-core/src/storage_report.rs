//! Exact allocation and catalog-file evidence is informational; unlike populations are never added together.
use crate::{FileScope, SourceOutcome, SourceState, telemetry::*};
use serde::{Deserialize, Serialize};
use ts_rs::TS;
#[derive(Serialize, Deserialize, TS, Clone, Debug)]
#[ts(export)]
pub struct StorageField {
    pub key: String,
    pub label: String,
    pub value: Option<String>,
}
#[derive(Serialize, Deserialize, TS, Clone, Debug)]
#[ts(export)]
pub struct StorageEntry {
    pub source: String,
    pub observation_key: String,
    pub scope: FileScope,
    pub title: String,
    pub explanation: String,
    pub fields: Vec<StorageField>,
}
#[derive(Serialize, Deserialize, TS, Clone, Debug)]
#[ts(export)]
pub struct StorageReport {
    pub target: CollectionTarget,
    pub context: ObservationReadContext,
    pub native_sources: Vec<StateSourceCoverage>,
    pub sources: Vec<SourceOutcome>,
    pub source_state: SourceState,
    pub read_complete: bool,
    pub native_rows: u32,
    pub observed_rows: u32,
    pub presented_rows: u32,
    pub entries_limited: bool,
    pub title: String,
    pub summary: String,
    pub limitations: Vec<String>,
    pub entries: Vec<StorageEntry>,
}
