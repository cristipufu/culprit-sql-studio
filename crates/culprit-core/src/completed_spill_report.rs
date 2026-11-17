//! Completed cache statistics identify a statement incarnation, never a current request or spilling operator.
use crate::{Metric, SourceOutcome, telemetry::*};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Clone, Debug, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct CompletedSpillReport {
    pub context: NativeReadContext,
    pub sources: Vec<SourceOutcome>,
    pub read_complete: bool,
    pub native_rows: u32,
    pub observed_rows: u32,
    pub definition_context: Option<NativeReadContext>,
    pub definition_sources: Vec<SourceOutcome>,
    pub definition_read_complete: Option<bool>,
    pub title: String,
    pub summary: String,
    pub limitations: Vec<String>,
    pub entries: Vec<CompletedSpillEntry>,
}

#[derive(Clone, Debug, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct CompletedSpillEntry {
    pub observation: DiagnosticEvidenceRef,
    pub identity: CachedStatementIdentity,
    pub database_id: u32,
    pub origin: StatementOrigin,
    pub reset: crate::ResetMarker,
    pub monitoring_template_id: Option<String>,
    pub state: CompletedSpillState,
    pub statement_binding: CompletedSpillStatementBinding,
    pub definition: Option<DiagnosticEvidenceRef>,
    /// Bounded redacted presentation only. The receipt identifies the original full payload in its own context.
    pub statement_preview: Option<String>,
    /// Presentation truncation of a complete authenticated native body, never warehouse preview completeness.
    pub statement_preview_truncated: bool,
    pub statement_unavailable: Option<PayloadUnavailable>,
    pub title: String,
    pub explanation: String,
    pub metrics: Vec<Metric>,
    pub investigation: String,
    pub verify: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum CompletedSpillState {
    RecordedSpills,
    NoRecordedSpills,
    Unavailable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum CompletedSpillStatementBinding {
    NotRequested,
    Matched,
    DefinitionNotObserved,
    SourceUnverified,
    SourceChanged,
}
