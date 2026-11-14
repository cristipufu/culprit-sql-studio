//! Explanations over an authenticated resource capture retain the native rows once, without creating a database.
use crate::{
    MemoryParticipantsReport, PageWaitReport, RetainedFailureReport, RuntimeDistributionReport, StatementWorkReport,
    TaskSpaceReport, collected_analysis::CollectedEvidenceContext,
};
use serde::Serialize;
use ts_rs::TS;

#[derive(Clone, Debug, Serialize, TS)]
#[ts(export)]
pub struct CollectedRetainedFailureAnalysis {
    pub context: CollectedEvidenceContext,
    pub report: RetainedFailureReport,
}

#[derive(Clone, Debug, Serialize, TS)]
#[ts(export)]
pub struct CollectedMemoryParticipantsAnalysis {
    pub context: CollectedEvidenceContext,
    pub report: MemoryParticipantsReport,
}

#[derive(Clone, Debug, Serialize, TS)]
#[ts(export)]
pub struct CollectedPageWaitAnalysis {
    pub context: CollectedEvidenceContext,
    pub report: PageWaitReport,
}

#[derive(Clone, Debug, Serialize, TS)]
#[ts(export)]
pub struct CollectedStatementWorkAnalysis {
    pub context: CollectedEvidenceContext,
    pub report: StatementWorkReport,
}

#[derive(Clone, Debug, Serialize, TS)]
#[ts(export)]
pub struct CollectedTaskSpaceAnalysis {
    pub context: CollectedEvidenceContext,
    pub report: TaskSpaceReport,
}

#[derive(Clone, Debug, Serialize, TS)]
#[ts(export)]
pub struct CollectedRuntimeDistributionAnalysis {
    pub context: CollectedEvidenceContext,
    pub report: RuntimeDistributionReport,
}
