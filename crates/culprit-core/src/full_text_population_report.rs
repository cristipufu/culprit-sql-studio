//! Informational original full-text population state; no grading, clock conversion or event continuity.
use crate::{Metric, SourceOutcome, telemetry::*};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Serialize, Deserialize, TS, Clone, Debug)]
#[ts(export)]
pub struct FullTextPopulationEntry {
    pub observation_key: String,
    pub position: u32,
    pub observed_at: String,
    pub status: Option<i32>,
    pub summary: String,
    pub metrics: Vec<Metric>,
}
#[derive(Serialize, Deserialize, TS, Clone, Debug)]
#[ts(export)]
pub struct FullTextPopulationOrigin {
    pub metadata: MssqlFullTextPopulationMetadata,
    pub context: NativeReadContext,
    pub target: MssqlFullTextPopulationTarget,
}
#[derive(Serialize, Deserialize, TS, Clone, Debug)]
#[ts(export)]
pub struct FullTextPopulationReport {
    pub original: Option<FullTextPopulationOrigin>,
    pub source: SourceOutcome,
    pub read_complete: bool,
    pub native_rows: Option<u32>,
    pub observed_rows: u32,
    pub title: String,
    pub summary: String,
    pub limitations: Vec<String>,
    pub investigation: String,
    pub verify: String,
    pub entries: Vec<FullTextPopulationEntry>,
}
