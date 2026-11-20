//! Informational catalog details, separate from full-text query outcomes and population progress.
use crate::{Metric, SourceState};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Serialize, Deserialize, TS, Clone, Debug)]
#[ts(export)]
pub struct TableFullTextEntry {
    pub schema: String,
    pub table: String,
    /// Present only for an original native Operational row, not a reconstructed scan position.
    pub observation_key: Option<String>,
    pub summary: String,
    pub metrics: Vec<Metric>,
    pub qualification: String,
    pub verify: String,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug)]
#[ts(export)]
pub struct TableFullTextReport {
    pub title: String,
    pub summary: String,
    pub source_state: Option<SourceState>,
    pub source_reason: Option<String>,
    pub observed_from: Option<String>,
    pub observed_until: Option<String>,
    pub native_rows: Option<u32>,
    pub observed_rows: u32,
    pub read_complete: Option<bool>,
    pub entries: Vec<TableFullTextEntry>,
    pub limitations: Vec<String>,
}
