//! One informational interpretation of selected current catalog facts; no rowgroup maintenance verdict.
use crate::{Metric, SourceOutcome, telemetry::*};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Serialize, Deserialize, TS, Clone, Debug)]
#[ts(export)]
pub struct ColumnstoreRowgroupEntry {
    pub observation_key: String,
    pub title: String,
    pub observed_at: String,
    pub partition_number: u32,
    pub row_group_id: u32,
    pub state: u8,
    pub metrics: Vec<Metric>,
}
#[derive(Serialize, Deserialize, TS, Clone, Debug)]
#[ts(export)]
pub struct ColumnstoreRowgroupReport {
    pub context: NativeReadContext,
    pub target: MssqlColumnstoreTarget,
    pub source: SourceOutcome,
    pub read_complete: bool,
    pub native_rows: u32,
    pub observed_rows: u32,
    pub title: String,
    pub summary: String,
    pub limitations: Vec<String>,
    pub investigation: String,
    pub verify: String,
    pub entries: Vec<ColumnstoreRowgroupEntry>,
}
