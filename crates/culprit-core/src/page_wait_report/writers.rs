//! A selected plan's object/index write access is distinct from physical-page contribution or latch ownership.
use crate::{MaintenanceClassification, SourceState, telemetry::*};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Serialize, Deserialize, TS, Clone, Debug)]
#[ts(export)]
pub struct PageWaitWriterReport {
    pub source_state: Option<SourceState>,
    pub source_reason: Option<String>,
    pub original_page: DiagnosticEvidenceRef,
    pub focus_key: String,
    pub target_state: Option<PageWriterTargetState>,
    pub title: String,
    pub summary: String,
    pub limitations: Vec<String>,
    pub entries: Vec<PageWaitWriterExplanation>,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug)]
#[ts(export)]
pub struct PageWaitWriterExplanation {
    pub parent: DiagnosticEvidenceRef,
    pub request: ActivityKey,
    pub statement: NativeIdentity,
    pub request_state: PageWaitRequestState,
    pub relation: PageWriterRelation,
    pub plan_identity: Option<NativeIdentity>,
    pub accesses: Vec<MssqlPageWriteAccess>,
    /// Current guarded SQL can identify maintenance syntax. This does not identify who acquired a latch or
    /// establish that this statement allocated earlier temporary objects or caused another request's wait.
    pub maintenance: MaintenanceClassification,
    pub title: String,
    pub explanation: String,
    pub investigation: String,
    pub verify: String,
}
