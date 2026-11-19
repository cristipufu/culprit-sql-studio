pub mod actual;
pub mod advice;
pub mod blocking;
pub mod blocking_report;
pub mod cache_entry_memory_report;
pub mod cached_plan_report;
pub mod changes;
pub mod check;
pub mod cleanup_report;
pub mod cloud;
pub mod collected_analysis;
pub mod collected_resource_analysis;
pub mod compilation;
pub mod compilation_report;
pub mod compiler_admission_report;
pub mod wait_counter_report;
pub use wait_counter_report::{WaitCounterEntry, WaitCounterIssue, WaitCounterReport};
pub mod counter_ratio_report;
pub use counter_ratio_report::{CounterRatioEntry, CounterRatioReport};
pub mod completed_spill_report;
mod configured_log_rate_report;
mod instance_resource_report;
pub use instance_resource_report::{InstanceResourceField, InstanceResourceGroup, InstanceResourceReport};
mod database_properties_report;
mod hyperscale_log_component_report;
mod memory_clerk_report;
mod performance_counter_report;
pub use database_properties_report::{DatabasePropertiesReport, DatabasePropertyField};
pub use memory_clerk_report::{MemoryClerkEntry, MemoryClerkField, MemoryClerkReport};
pub use performance_counter_report::{
    PerformanceCounterAvailability, PerformanceCounterEntry, PerformanceCounterReport,
};
pub mod connection;
pub mod connection_outcome_report;
pub mod connection_outcomes;
pub mod connection_security;
pub mod counter_rates;
pub mod coverage;
pub mod current_request_work_report;
pub mod dashboard;
pub mod deadlock;
pub mod dialect;
pub mod digest;
pub mod drift;
pub mod engines;
pub mod error;
pub mod events;
pub mod finding;
pub mod finding_facts;
pub mod fingerprint;
pub mod format;
pub mod i18n;
pub mod index_filter;
pub mod insights;
pub mod io_accounting;
pub mod jobs_report;
#[cfg(test)]
mod label_tests;
pub mod limits;
pub mod maintenance;
pub mod maintenance_blocking_report;
pub mod maintenance_progress_report;
pub mod measurement;
pub mod memory_participants;
pub mod model;
pub mod native_log_analysis;
pub mod operations;
mod storage_report;
pub use storage_report::{StorageEntry, StorageField, StorageReport};
mod session_census_report;
pub use session_census_report::{SessionCensusEntry, SessionCensusField, SessionCensusReport};
pub mod page_server_read_report;
pub mod page_wait_report;
pub mod plan;
pub mod plan_compare;
pub mod plan_diff;
pub mod plan_facts;
#[cfg(test)]
mod plan_facts_tests;
pub mod plan_view;
pub mod platform;
pub mod provenance;
pub mod provider;
pub mod query_store_statement_report;
pub mod query_temporary_work;
pub mod redact;
pub mod refinement;
pub mod replication_report;
pub mod retained_failure_report;
pub mod runtime_distribution;
pub mod runtime_distribution_report;
pub mod runtime_population;
pub mod serialization;
pub mod signals;
pub mod source_overhead_report;
pub mod sql;
pub mod statement_grant_report;
pub mod statement_work_report;
pub mod task_space_report;
pub mod telemetry;
pub mod tuning_report;
pub mod verification;
pub mod wait_resources;
pub mod waits;
pub mod window;
pub mod xml;

pub use actual::page_server_io::{PlanPageServerIo, PlanPageServerIoIssue, PlanPageServerMetric};
pub use actual::{
    ActualSummary, NodeProgress, NodeRuntime, NodeState, QueryProgress, SessionProgressContext, actual_summary,
    self_times,
};
pub use advice::*;
pub use blocking::{BlockNode, Blocking, BlockingChain, HeadKind, LockWait};
pub use blocking_report::BlockingReport;
pub use cache_entry_memory_report::{CacheEntryContextDifference, CacheEntryMemoryExplanation, CacheEntryMemoryReport};
pub use cached_plan_report::{
    CachedPlanEntry, CachedPlanIdentity, CachedPlanLimit, CachedPlanLimitation, CachedPlanPayload, CachedPlanReport,
    CollectedCachedPlanAnalysis,
};
pub use changes::{ChangeStatus, Changes, Period, PeriodStats, PlanSwitch, QueryChange};
pub use check::{Check, CheckAssessment, CheckEvidence, CheckStatus, Diagnosis};
pub use cleanup_report::{CleanupBoundary, CleanupExplanation, CleanupMechanism, CleanupReference, CleanupReport};
pub use cloud::{
    CloudLimitKind, CloudPoint, CloudProvider, CloudSeries, CloudSignals, CloudUnit, ThrottleEpisode, ThrottleKind,
};
pub use compilation::CompilationMemory;
pub use compilation_report::{
    CompilationChange, CompilationComparisonIssue, CompilationComparisonReport, CompilationExplanation,
    CompilationReport,
};
pub use compiler_admission_report::{CompilerAdmissionReport, CompilerGatewayExplanation, CompilerWaiterExplanation};
pub use completed_spill_report::{
    CompletedSpillEntry, CompletedSpillReference, CompletedSpillReport, CompletedSpillState,
    CompletedSpillStatementBinding,
};
pub use configured_log_rate_report::*;
pub use connection::{
    AuthKind, ConnectionAttempt, ConnectionEndpoint, ConnectionSpec, ConnectionTransport, SshConfig,
    aws_iam_host_allowed, aws_region_valid, entra_host_allowed, expand_home, is_local_host, transport_error,
};
pub use connection_outcome_report::*;
pub use connection_outcomes::*;
pub use connection_security::{
    ConnectionSecurity, ConnectionSecurityInfo, ConnectionSecurityMode, ConnectionSecuritySpec,
    apply_connection_security, connection_security,
};
pub use coverage::{Coverage, Source, SourceOutcome, SourceState, same_source, source_id};
pub use current_request_work_report::{CurrentRequestWorkEntry, CurrentRequestWorkReport};
pub use dashboard::{Dashboard, DashboardLive, ItemKind, Tile, TileItem, TileKey, TileMetric, TileStatus};
pub use deadlock::{
    Deadlock, DeadlockFix, DeadlockFixKind, DeadlockPattern, DeadlockProcess, DeadlockReport, DeadlockResource,
    LockHolder,
};
pub use dialect::{Dialect, NewIndex, Online};
pub use digest::{
    Digest, DigestChanges, DigestCounts, DigestDay, DigestDeadlockPattern, DigestDeadlocks, DigestDetail,
    DigestFinding, DigestIndexAdvice, DigestIndexCreate, DigestIndexDrop, DigestMetric, DigestQuery, DigestTopQuery,
};
pub use drift::{Drift, DriftItem, DriftKind, DriftPresence, DriftSide, server_label};
pub use engines::{
    CleanupEvidence, DatabaseProduct, EngineSpec, PlanningStep, ProductFacts, ProductSpec, ReadUnit, SettingChange,
    SqlSyntax, WriteLog, WriteRows, ysql_session_target,
};
pub use error::{Error, ErrorCode, ErrorDetail, Result};
pub use events::{ChangeEvent, EventAction, EventKind, EventLink, EventSource};
pub use finding::*;
pub use finding_facts::*;
pub use hyperscale_log_component_report::*;
pub use io_accounting::{FileIoContext, IoAccounting};
pub use jobs_report::{JobEntry, JobEntryKind, JobsReport};
pub use maintenance::*;
pub use maintenance_blocking_report::MaintenanceBlockingReport;
pub use maintenance_progress_report::{MaintenanceProgressEntry, MaintenanceProgressReport};
pub use measurement::*;
pub use memory_participants::{
    MemoryGrantAdmission, MemoryParticipantExplanation, MemoryParticipantMechanism, MemoryParticipantReference,
    MemoryParticipantsReport,
};
pub use model::*;
pub use native_log_analysis::{
    NativeLogAnalysis, NativeLogArtifact, NativeLogFormat, NativeLogIssue, NativeLogLimits, NativeLogProblem,
};
pub use operations::{
    MemoryLimit, NodeResource, NodeResources, ProcessMemory, QueryTermination, TerminationHistory, TerminationReason,
};
pub use page_wait_report::{
    PageTaskHolderBound, PageTaskHolderExplanation, PageTaskHolderReport, PageWaitActivityEvidence,
    PageWaitActivityReference, PageWaitBlockingBasis, PageWaitBlockingContext, PageWaitBlockingIssue,
    PageWaitBlockingParticipation, PageWaitExplanation, PageWaitRecordedIncident, PageWaitReport,
    PageWaitTaskReference, PageWaitWriterExplanation, PageWaitWriterReport, page_wait_blocking_participation,
};
pub use plan::*;
pub use plan_compare::{CompareContext, Scenario, ScenarioKind};
pub use plan_diff::{
    ChangeKind, HypotheticalIndexVerification, NodeMatch, PlanChange, PlanDiff, PlanRegion, plan_cost_basis, plan_diff,
    plan_diff_in, plan_diff_named, verify_hypothetical_index,
};
pub use plan_facts::{
    PlanAutomaticIndex, PlanChangedColumn, PlanConversion, PlanConversionIssue, PlanExecutionMode, PlanFacts,
    PlanIndexColumns, PlanMemoryGrantFeedback, PlanMemoryGrantFeedbackIssue, PlanMemoryGrantFeedbackState,
    PlanNodeFacts, PlanParentRelationship, PlanSampleSource,
};
pub use plan_view::{PlanInsightShare, PlanOperatorView, PlanPageServerIoView, PlanView};
pub use platform::Platform;
pub use provenance::{
    AnalysisExtension, AnalysisOrigin, AnalysisProvenance, AnalysisStage, BaselinePurpose, BaselineReference,
    CollectionProvenance, ComponentVersion, PipelineProvenance, ReportEvidence, ScanReference,
};
pub use provider::{ExplainMode, Provider, SnapshotOptions};
pub use query_temporary_work::{StatementTemporaryWork, TemporaryWorkExplanation};
pub use refinement::{FixWithholdingBasis, HistoryBasis, PresentationAspect, RefinementDecision, RefinementOutcome};
pub use replication_report::*;
pub use retained_failure_report::{RetainedFailureExplanation, RetainedFailureReport};
pub use runtime_distribution::*;
pub use runtime_distribution_report::*;
pub use runtime_population::*;
pub use signals::{
    ColumnHistogram, ConstraintIssue, ConstraintKind, ContentionBaseline, CountBaseline, EscalationBaseline, FileIo,
    FileKind, FileScope, FileWriteOperation, IndexContention, IndexEscalationCounts, IndexLockCounts, QueryCosts,
    Replica, ReplicaCondition, ReplicaRole, SizeBaseline, TableStorage, escalations_by_table,
};
pub use source_overhead_report::{
    CollectedSourceOverheadAnalysis, SourceNativeIntervalExplanation, SourceOverheadCaptureSummary,
    SourceOverheadEvidence, SourceOverheadReport,
};
pub use sql::{ColumnSort, SortDirection, TablePreview, TablePreviewOptions};
pub use statement_grant_report::{StatementGrantExplanation, StatementGrantRead, StatementGrantReport};
pub use statement_work_report::{StatementOutcomeCounts, StatementWorkEntry, StatementWorkReport};
pub use task_space_report::*;
pub use telemetry::{FileSize, FileSizeLimit};
pub use tuning_report::{TuningExplanation, TuningReport};
pub use verification::{Verification, VerifiedFinding, VerifyStatus, compare_reports};
pub use wait_resources::{ObservedWaitResource, WaitResourceKind};
pub use waits::{
    ActivityCategory, ActivityEvent, ActivityHistory, ActivityPeriod, ActivityQuery, ActivityReport, CleanupHolder,
    OpenTransaction, Pressure, QueryWaitCoverage, Signal, SignalScope, SignalStatus, SignalUnit, TransactionActivity,
    WaitCategory, WaitQuery, WaitSample, WaitScope, WaitsReport, signal_id,
};
pub use window::{MAX_WINDOW_MINUTES, MINUTES_PER_DAY, parse_window, window_label};

mod recipe_qualification;
pub use recipe_qualification::*;
