//! Exact original runtime fragments retain native endpoints independently of any workload aggregation.
use crate::{Metric, SourceState, telemetry::*};
use serde::{Deserialize, Serialize};
use ts_rs::TS;
#[derive(Serialize, Deserialize, TS, Clone, Debug)]
#[ts(export)]
pub struct QueryRuntimeFragmentMetric {
    pub key: RuntimeMetricKey,
    pub unit: RuntimeMetricUnit,
    pub title: String,
    pub statistics: Vec<Metric>,
}
#[derive(Serialize, Deserialize, TS, Clone, Debug)]
#[ts(export)]
pub struct QueryRuntimeFragmentEntry {
    pub reference: CapturedRecordReference,
    pub native: NativeMeasurementKey,
    pub executions: String,
    /// Original native execution timestamps; source semantics stay separate from acquisition clocks.
    pub first_execution_at: Option<String>,
    pub last_execution_at: Option<String>,
    pub metrics: Vec<QueryRuntimeFragmentMetric>,
    /// Existing five native page-server statistics retain their independent availability and units.
    pub page_server_reads: Option<Box<PageServerReadStatistics>>,
    pub page_server_explanation: Option<Box<crate::page_server_read_report::PageServerReadExplanation>>,
}
#[derive(Serialize, Deserialize, TS, Clone, Debug)]
#[ts(export)]
pub struct QueryRuntimeFragmentReport {
    pub source: String,
    pub source_state: SourceState,
    pub source_reason: Option<String>,
    pub title: String,
    pub summary: String,
    pub limitations: Vec<String>,
    pub selected_rows: Option<u32>,
    pub observed_rows: u32,
    pub read_complete: bool,
    pub references_complete: Option<bool>,
    pub presentation_limited: bool,
    pub read: Option<Box<CapturedReadRef>>,
    pub description: Option<Box<CollectionDescription>>,
    pub collected_from: Option<String>,
    pub collected_until: Option<String>,
    pub entries: Vec<QueryRuntimeFragmentEntry>,
}
