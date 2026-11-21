//! Historical native intervals, not a current pressure point or a database/query CPU allocation.
use crate::{SourceState, telemetry::*};
use serde::{Deserialize, Serialize};
use ts_rs::TS;
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct ManagedInstanceResourceField {
    pub key: String,
    pub label: String,
    pub availability: MssqlCatalogAvailability,
    pub native_value: Option<String>,
    pub value: Option<String>,
    pub missing_reason: Option<String>,
}
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct ManagedInstanceResourceInterval {
    pub started_at: String,
    pub ended_at: String,
    pub fields: Vec<ManagedInstanceResourceField>,
}
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct ManagedInstanceResourceReport {
    pub title: String,
    pub summary: String,
    pub source_state: SourceState,
    pub native_source: Option<StateSourceCoverage>,
    pub read_complete: bool,
    pub from: Option<String>,
    pub until: Option<String>,
    /// Original native reader clock, never the publication head or latest represented interval.
    pub observed_at: Option<String>,
    pub native_rows: Option<u32>,
    pub recorded_rows: Option<u32>,
    pub observed_rows: u32,
    pub layout: Option<MssqlManagedInstanceResourceLayout>,
    pub intervals: Vec<ManagedInstanceResourceInterval>,
    pub limitations: Vec<String>,
    pub investigation: String,
    pub verify: String,
}
