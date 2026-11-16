//! Explanations over an authenticated resource capture retain the native rows once, without creating a database.
use crate::{
    MemoryParticipantsReport, PageWaitReport, RetainedFailureReport, RuntimeDistributionReport, StatementWorkReport,
    TaskSpaceReport, collected_analysis::CollectedEvidenceContext,
};
use serde::Serialize;
use ts_rs::TS;

#[derive(Clone, Debug, Serialize, TS)]
#[ts(export)]
pub struct CollectedCleanupAnalysis {
    pub context: CollectedEvidenceContext,
    pub report: crate::CleanupReport,
}

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
    /// Optional original Activity evidence, retained once and independently from the later page lookup.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub activity_context: Option<CollectedEvidenceContext>,
    pub report: PageWaitReport,
}

#[derive(Clone, Debug, Serialize, TS)]
#[ts(export)]
pub struct CollectedPageWaitWriterAnalysis {
    pub context: CollectedEvidenceContext,
    pub report: crate::PageWaitWriterReport,
}

#[derive(Clone, Debug, Serialize, TS)]
#[ts(export)]
pub struct CollectedStatementWorkAnalysis {
    pub context: CollectedEvidenceContext,
    pub report: StatementWorkReport,
}

#[derive(Clone, Debug, Serialize, TS)]
#[ts(export)]
pub struct CollectedStatementGrantAnalysis {
    pub context: CollectedEvidenceContext,
    pub report: crate::StatementGrantReport,
}

#[derive(Clone, Debug, Serialize, TS)]
#[ts(export)]
pub struct CollectedCacheEntryMemoryAnalysis {
    pub context: CollectedEvidenceContext,
    pub report: crate::CacheEntryMemoryReport,
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

#[derive(Clone, Debug, Serialize, TS)]
#[ts(export)]
pub struct CollectedCompilationAnalysis {
    pub context: CollectedEvidenceContext,
    pub report: crate::CompilationReport,
}

#[derive(Clone, Debug, Serialize, TS)]
#[ts(export)]
pub struct CollectedCompilerAdmissionAnalysis {
    pub context: CollectedEvidenceContext,
    pub report: crate::CompilerAdmissionReport,
}

#[derive(Clone, Debug, Serialize, TS)]
#[ts(export)]
pub struct CollectedMaintenanceAnalysis {
    pub context: CollectedEvidenceContext,
    pub report: crate::MaintenanceReport,
}

#[derive(Clone, Debug, Serialize, TS)]
#[ts(export)]
pub struct CollectedMaintenanceProgressAnalysis {
    pub context: CollectedEvidenceContext,
    pub report: crate::MaintenanceProgressReport,
}

#[derive(Clone, Debug, Serialize, TS)]
#[ts(export)]
pub struct CollectedTuningAnalysis {
    pub context: CollectedEvidenceContext,
    pub report: crate::TuningReport,
}
