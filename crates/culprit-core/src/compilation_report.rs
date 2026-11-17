//! Query compilation explanations reference the retained native facts, independently of execution or current memory.
use crate::{
    Metric, SourceState,
    telemetry::{DiagnosticEvidenceRef, NativeQueryIdentity, PayloadUnavailable, StatementOrigin},
};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Serialize, Deserialize, TS, Clone, Debug)]
#[ts(export)]
pub struct CompilationReport {
    pub source_state: Option<SourceState>,
    pub source_reason: Option<String>,
    pub title: String,
    pub summary: String,
    pub limitations: Vec<String>,
    pub entries: Vec<CompilationExplanation>,
    /// The optional definition selection has its own archive coverage. Its runtime history interval is not an
    /// acquisition clock or a compilation population; the SDK retains the complete original capture context.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub definition_read: Option<Box<CompilationDefinitionRead>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub comparison: Option<CompilationComparisonReport>,
}

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum CompilationComparisonIssue {
    NativeContextUnavailable,
    NativeNamespaceUnavailable,
    DifferentSource,
    InvalidObservationOrder,
    SourceUnavailable,
    PartialRead,
    UnmatchedEntries,
    CounterChanged,
    NativeClockConflict,
}

/// Exact count changes have their own native observation bracket. They carry no duration, memory total or
/// execution-population share. Original archived capture proofs are retained by the SDK's enclosing context.
#[derive(Serialize, Deserialize, TS, Clone, Debug)]
#[ts(export)]
pub struct CompilationComparisonReport {
    pub baseline: Option<crate::ScanReference>,
    pub observed_from: Option<String>,
    pub observed_until: Option<String>,
    pub missing_before: u32,
    pub missing_after: u32,
    pub issues: Vec<CompilationComparisonIssue>,
    pub title: String,
    pub summary: String,
    pub limitations: Vec<String>,
    pub entries: Vec<CompilationChange>,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug)]
#[ts(export)]
pub struct CompilationChange {
    /// Each side's exact native observation key; they are equal only within the separately qualified read pair.
    pub observation_key: String,
    pub baseline_observation_key: String,
    pub identity: NativeQueryIdentity,
    pub compilations: String,
    pub last_compile_at: String,
    pub explanation: String,
    pub investigation: String,
    pub verify: String,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug)]
#[ts(export)]
pub struct CompilationExplanation {
    pub observation_key: String,
    pub identity: NativeQueryIdentity,
    pub title: String,
    pub explanation: String,
    pub metrics: Vec<Metric>,
    pub investigation: String,
    pub verify: String,
    /// Derived statement authority, separate from the immutable native compiler counters and their origin.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub statement: Option<Box<CompilationStatementExplanation>>,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug)]
#[ts(export)]
pub struct CompilationDefinitionRead {
    pub namespace: CompilationDefinitionNamespace,
    /// Selected authenticated manifest rows, never the entire Query Store query population.
    pub selected_rows: u32,
    pub observed_rows: u32,
    pub read_complete: bool,
}

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum CompilationDefinitionNamespace {
    Matched,
    Unverified,
    Changed,
}

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum CompilationStatementBinding {
    Captured,
    Matched,
    NotObserved,
    SourceUnverified,
    SourceChanged,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug)]
#[ts(export)]
pub struct CompilationStatementExplanation {
    pub binding: CompilationStatementBinding,
    pub definition: Option<DiagnosticEvidenceRef>,
    pub origin: StatementOrigin,
    pub monitoring_template_id: Option<String>,
    pub preview: Option<String>,
    /// Presentation truncation of a complete verified body. A warehouse prefix never supplies this authority.
    pub preview_truncated: bool,
    pub unavailable: Option<PayloadUnavailable>,
    pub explanation: String,
}
