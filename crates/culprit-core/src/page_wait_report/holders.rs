//! Reported task holders are point relationships, independently of planned writers and measured contribution.
use crate::{SourceState, telemetry::*};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Serialize, Deserialize, TS, Clone, Debug)]
#[ts(export)]
pub struct PageTaskHolderReport {
    pub source_state: Option<SourceState>,
    pub source_reason: Option<String>,
    pub original_page: DiagnosticEvidenceRef,
    pub focus_key: String,
    pub target_state: Option<PageWriterTargetState>,
    pub final_target: Option<Box<PageTaskHolderTargetCheck>>,
    pub title: String,
    pub summary: String,
    pub limitations: Vec<String>,
    pub entries: Vec<PageTaskHolderExplanation>,
}

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum PageTaskHolderBound {
    Before,
    After,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug)]
#[ts(export)]
pub struct PageTaskHolderExplanation {
    pub bound: PageTaskHolderBound,
    pub observed_at: String,
    pub request_state: PageWaitRequestState,
    pub wait_state: PageWaitRelation,
    pub relation: PageTaskHolderRelation,
    pub task: Option<MssqlPageTaskIdentity>,
    pub request: Option<ActivityKey>,
    pub statement_identity: Option<NativeIdentity>,
    pub statement: Option<DefinitionPayload>,
    pub title: String,
    pub explanation: String,
    pub investigation: String,
    pub verify: String,
}
