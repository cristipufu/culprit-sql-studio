//! Explanations retain the original runtime/plan identity. Remote read statistics establish no current owner.
use crate::{
    Metric, SourceState,
    collected_analysis::CollectedEvidenceContext,
    query_store_statement_report::{QueryStoreDefinitionRead, QueryStoreStatementExplanation},
    telemetry::{
        CapturedReadRef, CapturedRecordReference, NativeIdentity, NativeMeasurementKey, NativePlanIdentity,
        PageServerReadAvailability, ReferenceCoverage,
    },
};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Serialize, Deserialize, TS, Clone, Debug)]
#[ts(export)]
pub struct PageServerReadExplanation {
    pub availability: PageServerReadAvailability,
    pub title: String,
    pub summary: String,
    pub metrics: Vec<Metric>,
    pub limitations: Vec<String>,
    pub investigate: String,
    pub verify: String,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug)]
#[serde(tag = "kind", rename_all = "kebab-case")]
#[ts(export)]
pub enum PageServerReadEntryIdentity {
    RuntimeFragment {
        reference: Box<CapturedRecordReference>,
        native: Box<NativeMeasurementKey>,
    },
    RecordedPlan {
        query_label: String,
        plan_label: String,
        native_plan: Option<Box<NativePlanIdentity>>,
        native_fragment: Option<Box<NativeIdentity>>,
        native_fragments: String,
        execution_type: Option<String>,
    },
}

#[derive(Serialize, Deserialize, TS, Clone, Debug)]
#[ts(export)]
pub struct PageServerReadEntry {
    pub identity: PageServerReadEntryIdentity,
    pub executions: String,
    pub explanation: PageServerReadExplanation,
    /// Optional independently authenticated original query definition; native statistics remain unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub statement: Option<Box<QueryStoreStatementExplanation>>,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug)]
#[ts(export)]
pub struct PageServerReadReport {
    pub source: String,
    /// Original source capability, distinct from whether any selected execution rows exist.
    pub availability: Option<PageServerReadAvailability>,
    /// Archive completeness is limited to the selected retained rows; scan completeness to its retained plans.
    pub read_complete: bool,
    pub selected_rows: Option<u32>,
    pub observed_rows: u32,
    pub presentation_limited: bool,
    pub source_state: Option<SourceState>,
    pub source_reason: Option<String>,
    pub title: String,
    pub summary: String,
    pub limitations: Vec<String>,
    pub entries: Vec<PageServerReadEntry>,
    /// Original runtime capture/source and stream counts, retained once for all captured row positions.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub runtime_read: Option<Box<CapturedReadRef>>,
    /// Original semantic/native reference coverage, independent of this archive body's read completeness.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub reference_coverage: Option<Box<ReferenceCoverage>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub definition_read: Option<Box<QueryStoreDefinitionRead>>,
}

#[derive(Serialize, TS, Clone, Debug)]
#[ts(export)]
pub struct CollectedPageServerReadsAnalysis {
    pub context: CollectedEvidenceContext,
    /// Original definition source, complete native rows, receipts, gaps and archive acquisition bounds.
    pub definition_context: Option<Box<CollectedEvidenceContext>>,
    pub report: PageServerReadReport,
}
