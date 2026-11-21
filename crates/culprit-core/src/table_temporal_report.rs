//! Persisted same-table settings, separate from measured cleanup, retained rows and parent ownership.
use crate::{SourceState, telemetry::MssqlCatalogAvailability};
use serde::{Deserialize, Serialize};
use ts_rs::TS;
#[derive(Serialize, Deserialize, TS, Clone, Debug)]
#[ts(export)]
pub struct TableTemporalField {
    pub key: String,
    pub label: String,
    pub text: String,
    pub availability: MssqlCatalogAvailability,
}
#[derive(Serialize, Deserialize, TS, Clone, Debug)]
#[ts(export)]
pub struct TableTemporalEntry {
    pub schema: String,
    pub table: String,
    pub observation_key: Option<String>,
    pub fields: Vec<TableTemporalField>,
}
#[derive(Serialize, Deserialize, TS, Clone, Debug)]
#[ts(export)]
pub struct TableTemporalReport {
    pub title: String,
    pub summary: String,
    pub source_state: Option<SourceState>,
    pub source_reason: Option<String>,
    pub observed_from: Option<String>,
    pub observed_until: Option<String>,
    pub native_rows: Option<u32>,
    pub observed_rows: u32,
    pub read_complete: Option<bool>,
    pub qualification: String,
    pub entries: Vec<TableTemporalEntry>,
    pub limitations: Vec<String>,
}
