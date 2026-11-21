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
/// One original complete selected-database catalog census, never effective log capacity or host disk space.
#[derive(Serialize, Deserialize, TS, Clone, Debug)]
#[ts(export)]
pub struct StorageFileSummary {
    pub database: String,
    pub source: String,
    pub source_state: SourceState,
    pub observed_at: Option<String>,
    pub native_database_id: Option<String>,
    pub data_file_count: Option<u32>,
    pub log_file_count: Option<u32>,
    pub other_file_count: Option<u32>,
    pub data_allocated_bytes: Option<String>,
    pub data_used_bytes: Option<String>,
    pub log_catalog_allocated_bytes: Option<String>,
    pub title: String,
    pub summary: String,
    pub fields: Vec<StorageField>,
    pub limitations: Vec<String>,
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
    pub file_summary: Option<Box<StorageFileSummary>>,
    pub title: String,
    pub summary: String,
    pub limitations: Vec<String>,
    pub entries: Vec<StorageEntry>,
}
