//! Read-only selected revision history. These display values never authorize source work or session actions.
use crate::{
    MeasurementScope, SourceState,
    telemetry::{DiagnosticAnalysisStatus, DiagnosticCaptureRef, DiagnosticEvidenceRef, DiagnosticLifecycle},
};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Bounds the selected immutable revision population, independently of UI pagination and native collection limits.
pub const MAX_INCIDENT_HISTORY_REVISIONS: usize = 256;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum IncidentHistoryProvenance {
    /// The original capture reader authenticated the selected revision bodies, not every referenced payload.
    AuthenticatedCapture,
    /// Canonical JSONL was structurally checked. No manifest, archive or source authority was authenticated.
    ImportedArtifact,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct IncidentHistoryEvidence {
    pub label: String,
    pub capture: DiagnosticCaptureRef,
    pub reference: Option<DiagnosticEvidenceRef>,
    /// A reference is retained; its underlying payload was not reopened by this history read.
    pub availability: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct IncidentHistoryEntry {
    pub controller_version: u32,
    pub revision_id: String,
    pub episode_id: String,
    /// The existing canonical target ID preserves database/shared identity without exposing endpoints or paths.
    pub target_id: String,
    pub target_scope: MeasurementScope,
    pub scope_id: String,
    pub source: String,
    pub scope: MeasurementScope,
    pub scope_label: String,
    /// Controller revision order remains an exact decimal; JavaScript numbers cannot replace it.
    pub revision: String,
    pub previous_revision: Option<String>,
    pub predecessor_selected: bool,
    pub lifecycle: DiagnosticLifecycle,
    pub analysis: DiagnosticAnalysisStatus,
    pub title: String,
    pub phase: String,
    pub first_seen_at: String,
    pub last_seen_at: String,
    /// Collector analysis time, never an invented native recovery or event timestamp.
    pub recorded_at: String,
    pub original: Option<DiagnosticEvidenceRef>,
    pub evidence: Vec<IncidentHistoryEvidence>,
    pub input_references: u32,
    /// The recorded attempt's recipe, without its private focus/approval or any executable authority.
    pub followup_recipe: Option<String>,
    pub followup_recipe_version: Option<u32>,
    pub issue: Option<crate::telemetry::DiagnosticAnalysisIssue>,
    pub reasons: Vec<String>,
    /// Historical guidance only; no executable session action or fresh-source authority is carried here.
    pub next_step: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct IncidentHistoryReport {
    pub provenance: IncidentHistoryProvenance,
    /// Complete reading of the selected input says nothing about unselected revisions/controller state.
    pub selected_read_complete: bool,
    pub source_state: Option<SourceState>,
    pub source_reason: Option<String>,
    pub title: String,
    pub summary: String,
    pub limitations: Vec<String>,
    pub entries: Vec<IncidentHistoryEntry>,
}
