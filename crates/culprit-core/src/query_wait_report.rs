//! Per-fragment native wait statistics never imply an ordering or a combined distribution across fragments.
use crate::{Metric, SourceState, telemetry::*};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Serialize, Deserialize, TS, Clone, Debug)]
#[ts(export)]
pub struct QueryWaitFragmentEntry {
    pub reference: CapturedRecordReference,
    pub native: NativeMeasurementKey,
    pub category: String,
    pub category_description: Option<String>,
    pub outcome_description: Option<String>,
    pub unit: RuntimeMetricUnit,
    pub metrics: Vec<Metric>,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug)]
#[ts(export)]
pub struct QueryWaitFragmentReport {
    pub source: String,
    pub source_state: SourceState,
    pub source_reason: Option<String>,
    pub title: String,
    pub summary: String,
    pub limitations: Vec<String>,
    pub selected_rows: Option<u32>,
    pub observed_rows: u32,
    pub read_complete: bool,
    /// Original native recording configuration is independent of body completeness and interval coverage.
    pub capture: Option<WaitCaptureState>,
    pub interval_coverage: Option<WaitCaptureCoverage>,
    pub references_complete: Option<bool>,
    pub presentation_limited: bool,
    /// Indivisible original metadata stays absent after sharing/withdrawal.
    pub read: Option<Box<CapturedReadRef>>,
    pub description: Option<Box<CollectionDescription>>,
    pub collected_from: Option<String>,
    pub collected_until: Option<String>,
    pub entries: Vec<QueryWaitFragmentEntry>,
}
