//! Explanations over an authenticated resource capture retain the native rows once, without creating a database.
use crate::{
    MemoryParticipantsReport, PageWaitReport, RetainedFailureReport, RuntimeDistributionReport, StatementWorkReport,
    TaskSpaceReport, collected_analysis::CollectedEvidenceContext,
};
use serde::Serialize;
use ts_rs::TS;

#[derive(Clone, Debug, Serialize, TS)]
#[ts(export)]
pub struct CollectedStorageAnalysis {
    pub context: CollectedEvidenceContext,
    pub report: Box<crate::StorageReport>,
}

#[derive(Clone, Debug, Serialize, TS)]
#[ts(export)]
pub struct CollectedSessionCensusAnalysis {
    pub context: CollectedEvidenceContext,
    pub report: Box<crate::SessionCensusReport>,
}

#[derive(Clone, Debug, Serialize, TS)]
#[ts(export)]
pub struct CollectedDatabasePropertiesAnalysis {
    pub context: CollectedEvidenceContext,
    pub report: Box<crate::DatabasePropertiesReport>,
}

#[derive(Clone, Debug, Serialize, TS)]
#[ts(export)]
pub struct CollectedBlockingAnalysis {
    pub context: CollectedEvidenceContext,
    pub report: Box<crate::BlockingReport>,
}

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
pub struct CollectedPageTaskHolderAnalysis {
    pub context: CollectedEvidenceContext,
    pub report: crate::PageTaskHolderReport,
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
    pub baseline_context: Option<CollectedEvidenceContext>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub baseline_context: Option<CollectedEvidenceContext>,
    pub report: crate::CompilationReport,
    /// Independently authenticated original definitions. Neither its runtime interval nor its client read clocks
    /// replace the native compiler acquisition bounds or baseline receipt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub definition_context: Option<Box<CollectedEvidenceContext>>,
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
pub struct CollectedMaintenanceBlockingAnalysis {
    pub maintenance_context: CollectedEvidenceContext,
    pub blocking_context: CollectedEvidenceContext,
    pub report: crate::MaintenanceBlockingReport,
}

#[derive(Clone, Debug, Serialize, TS)]
#[ts(export)]
pub struct CollectedCurrentRequestWorkAnalysis {
    pub context: CollectedEvidenceContext,
    pub report: crate::CurrentRequestWorkReport,
}

#[derive(Clone, Debug, Serialize, TS)]
#[ts(export)]
pub struct CollectedReplicationAnalysis {
    pub context: CollectedEvidenceContext,
    pub report: crate::ReplicationReport,
}

#[derive(Clone, Debug, Serialize, TS)]
#[ts(export)]
pub struct CollectedConnectionOutcomeAnalysis {
    pub context: CollectedEvidenceContext,
    pub report: Box<crate::ConnectionOutcomeReport>,
}

#[derive(Clone, Debug, Serialize, TS)]
#[ts(export)]
pub struct CollectedConfiguredLogRateAnalysis {
    pub context: CollectedEvidenceContext,
    pub report: Box<crate::ConfiguredLogRateReport>,
}

#[derive(Clone, Debug, Serialize, TS)]
#[ts(export)]
pub struct CollectedTuningAnalysis {
    pub context: CollectedEvidenceContext,
    pub report: crate::TuningReport,
}

#[derive(Clone, Debug, Serialize, TS)]
#[ts(export)]
pub struct CollectedJobsAnalysis {
    pub context: CollectedEvidenceContext,
    pub report: crate::JobsReport,
}

/// Each source keeps its own authenticated capture, clocks, read work and warehouse gaps.
#[derive(Clone, Debug, Serialize, TS)]
#[ts(export)]
pub struct CollectedCompletedSpillAnalysis {
    pub context: CollectedEvidenceContext,
    pub definition_context: Option<CollectedEvidenceContext>,
    pub report: crate::CompletedSpillReport,
}

#[derive(Clone, Debug, Serialize, TS)]
#[ts(export)]
pub struct CollectedCounterRatioAnalysis {
    pub context: CollectedEvidenceContext,
    pub report: crate::CounterRatioReport,
}

#[derive(Clone, Debug, Serialize, TS)]
#[ts(export)]
pub struct CollectedWaitCounterAnalysis {
    pub context: CollectedEvidenceContext,
    pub report: crate::WaitCounterReport,
}

#[derive(Clone, Debug, Serialize, TS)]
#[ts(export)]
pub struct CollectedMemoryClerkAnalysis {
    pub context: CollectedEvidenceContext,
    pub report: Box<crate::MemoryClerkReport>,
}

#[derive(Clone, Debug, Serialize, TS)]
#[ts(export)]
pub struct CollectedHyperscaleLogComponentAnalysis {
    pub context: CollectedEvidenceContext,
    pub report: Box<crate::HyperscaleLogComponentReport>,
}

#[derive(Clone, Debug, Serialize, TS)]
#[ts(export)]
pub struct CollectedPerformanceCounterAnalysis {
    pub context: CollectedEvidenceContext,
    pub report: Box<crate::PerformanceCounterReport>,
}

/// An independent original shared-instance point; no selected-database pressure or current-owner inference.
#[derive(serde::Serialize, ts_rs::TS, Clone, Debug)]
#[ts(export)]
pub struct CollectedInstanceResourceAnalysis {
    pub context: CollectedEvidenceContext,
    pub report: Box<crate::InstanceResourceReport>,
}

#[derive(Clone, Debug, Serialize, TS)]
#[ts(export)]
pub struct CollectedTempdbMetadataMemoryAnalysis {
    pub context: CollectedEvidenceContext,
    pub report: Box<crate::TempdbMetadataMemoryReport>,
}
