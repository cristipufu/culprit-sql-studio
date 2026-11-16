//! Current maintenance participation retains native scope and blocking direction, never statement lock ownership.
use crate::{
    Metric, SourceState,
    telemetry::{
        ActivityKey, ActivityPopulation, ActivityWaitOwner, DiagnosticEvidenceRef, NativeIdentity, NativeReadContext,
    },
};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

mod classification;
pub use classification::{MAX_MAINTENANCE_STATEMENT_BYTES, classify_maintenance};

pub const MAX_MAINTENANCE_OBSERVATIONS: usize = crate::telemetry::MAX_ACTIVITY_FRAME_OBSERVATIONS;
pub const MAX_MAINTENANCE_ENTRIES: usize = 256;
pub const MAX_MAINTENANCE_REFERENCES_PER_ENTRY: usize = 16;

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum MaintenanceKind {
    IndexCreate,
    IndexReplace,
    IndexRebuild,
    IndexReorganize,
    IndexReindex,
    Cluster,
    Vacuum,
    Statistics,
    TableOptimize,
    TableRepair,
}

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum MaintenanceClassificationIssue {
    InvalidObservation,
    NoCurrentRequest,
    StatementUnavailable,
    StatementIdentityUnavailable,
    StatementTooLarge,
    InvalidStatement,
}

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "kebab-case")]
#[ts(export)]
pub enum MaintenanceClassification {
    Maintenance { kind: MaintenanceKind },
    OtherStatement,
    Unresolved { reason: MaintenanceClassificationIssue },
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct MaintenanceReference {
    pub activity: ActivityKey,
    pub statement: Option<NativeIdentity>,
    /// Archive references identify the immutable original row. A direct native frame may have no archive receipt.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub evidence: Option<DiagnosticEvidenceRef>,
}

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum MaintenanceDirection {
    BlockingSession,
    Waiting,
    Both,
    Unlinked,
}

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum MaintenanceRelationIssue {
    NativeScopeUnqualified,
    SessionOwnershipOnly,
    MultipleOwnerRequests,
    WaitOwnerUnavailable,
    ReferencesLimited,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug)]
#[ts(export)]
pub struct MaintenanceParticipation {
    pub reference: MaintenanceReference,
    pub kind: MaintenanceKind,
    pub direction: MaintenanceDirection,
    /// Qualified native waiters identify a blocking session, not the request that originally acquired its locks.
    pub affected: Vec<MaintenanceReference>,
    pub affected_count: u32,
    pub waiting_on: Vec<ActivityWaitOwner>,
    pub competing_requests: Vec<MaintenanceReference>,
    pub competing_request_count: u32,
    pub issues: Vec<MaintenanceRelationIssue>,
    pub title: String,
    pub explanation: String,
    pub metrics: Vec<Metric>,
    pub investigation: String,
    pub verify: String,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug)]
#[ts(export)]
pub struct MaintenanceReport {
    pub source: String,
    pub context: NativeReadContext,
    pub native_epoch: Option<String>,
    pub population: ActivityPopulation,
    pub source_state: SourceState,
    pub source_reason: Option<String>,
    /// Warehouse/read completeness is independent of the original native population and source state.
    pub read_complete: bool,
    pub observed_rows: u32,
    pub native_rows: u32,
    pub maintenance_observations: u32,
    pub unclassified_requests: u32,
    pub entries: Vec<MaintenanceParticipation>,
    pub title: String,
    pub summary: String,
    pub limitations: Vec<String>,
}
