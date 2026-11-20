//! Informational display of one original retained-history point; it carries no severity or integrity verdict.
use crate::{Metric, SourceState, telemetry::*};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Serialize, Deserialize, TS, Clone, Debug)]
#[ts(export)]
pub struct SuspectPageHistoryReport {
    pub context: Option<ObservationReadContext>,
    pub native_source: Option<StateSourceCoverage>,
    pub source_state: Option<SourceState>,
    pub source_reason: Option<String>,
    pub read_complete: Option<bool>,
    pub database: Option<String>,
    pub native_database_id: Option<String>,
    pub observed_at: Option<String>,
    pub active_rows: Option<String>,
    pub retained_rows: Option<String>,
    pub last_reported_time_without_offset: Option<String>,
    pub title: String,
    pub summary: String,
    pub metrics: Vec<Metric>,
    pub limitations: Vec<String>,
}
