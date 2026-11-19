//! Page-target explanations reference observed waiters and metadata. They never assert transient latch ownership.
use crate::{
    ObservedWaitResource, SourceState,
    telemetry::{ActivityClient, ActivityKey, DefinitionPayload, DiagnosticEvidenceRef},
};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

mod participation;
pub use participation::*;
mod writers;
pub use writers::*;
mod holders;
pub use holders::*;

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct PageWaitReport {
    pub source_state: Option<SourceState>,
    pub source_reason: Option<String>,
    pub title: String,
    pub summary: String,
    /// Authenticated original waiter context, independent of a later changed or missing request/page lookup.
    /// This records an incident; it establishes no current statement, physical target or latch ownership.
    pub recorded_incident: Option<PageWaitRecordedIncident>,
    pub limitations: Vec<String>,
    pub entries: Vec<PageWaitExplanation>,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct PageWaitRecordedIncident {
    pub original: DiagnosticEvidenceRef,
    pub observed_at: String,
    pub request: ActivityKey,
    pub resource: ObservedWaitResource,
    pub sql: DefinitionPayload,
    pub client: Option<ActivityClient>,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct PageWaitExplanation {
    pub observation_key: String,
    /// Small exact references also locate the bare native row in a scan without implementing proof hashing in UI.
    pub request_key: ActivityKey,
    /// A capture-local task reference distinguishes parallel waiters sharing a request and page. It does not
    /// establish a task generation, latch owner or continuity between independently observed task slots.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub task: Option<PageWaitTaskReference>,
    pub resource: ObservedWaitResource,
    /// Historical engine-reported blocking-session context from the authenticated original Activity frame.
    /// This never identifies the request that acquired a latch, a present owner or a page-writing cause.
    pub blocking: PageWaitBlockingParticipation,
    pub title: String,
    pub explanation: String,
    pub investigation: String,
    pub verify: String,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct PageWaitTaskReference {
    pub task_address: String,
    pub execution_context_id: u32,
}

impl From<&crate::telemetry::MssqlPageTaskWait> for PageWaitTaskReference {
    fn from(task: &crate::telemetry::MssqlPageTaskWait) -> Self {
        Self { task_address: task.task_address.clone(), execution_context_id: task.execution_context_id }
    }
}
