//! An optional local-original reader is independent of native providers and warehouse adapters.
use culprit_core::{
    MemoryClerkReport, Result, RuntimePopulationMembership, limits::OperationHandle,
    page_server_read_report::PageServerReadReport, telemetry::CapturedOperationalRead,
};
use serde::{Deserialize, Serialize};
use std::{future::Future, pin::Pin, time::Duration};
use ts_rs::TS;

#[cfg(feature = "collector")]
mod outbox;
#[cfg(feature = "collector")]
pub use outbox::OutboxCaptureReader;
mod operational;
pub use operational::{LocalOperationalCapture, LocalSnapshotAnalysis, LocalSnapshotReport};
mod blocking;
pub use blocking::{LocalActivityRead, LocalBlockingAnalysis};
mod replication;
pub use replication::LocalReplication;
mod wait_counters;
pub use wait_counters::LocalWaitCounters;
mod pool_settings;
pub use pool_settings::LocalPoolSettings;
mod incident_history;
pub use incident_history::LocalIncidentHistory;
mod source_overhead;
pub use source_overhead::LocalSourceOverhead;
mod session_census;
pub use session_census::LocalSessionCensus;

/// A closed analysis choice; selecting an analysis starts no native collection or source discovery.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum LocalCaptureAnalysisKind {
    IncidentHistory,
    SourceOverhead,
    PageServerReads,
    QueryWaitFragments,
    RuntimeFragments,
    MemoryClerks,
    TempdbMetadataMemory,
    HyperscaleLogComponents,
    PerformanceCounters,
    DatabaseProperties,
    SuspectPages,
    TableFullText,
    TableTemporal,
    IndexConfiguration,
    IndexedViews,
    InstanceResources,
    ManagedInstanceResources,
    Compilation,
    ConfiguredLogRates,
    ConnectionOutcomes,
    Jobs,
    Cdc,
    ChangeFeed,
    ColumnstoreRowgroups,
    FullTextPopulations,
    TableMemory,
    GroupReplication,
    Replication,
    PoolSettings,
    WaitCounters,
    Galera,
    BackupHistory,
    RequestMemory,
    TaskSpace,
    CurrentRequestWork,
    StatementWork,
    MaintenanceProgress,
    CompletedSpills,
    Storage,
    Blocking,
    SessionCensus,
}
impl LocalCaptureAnalysisKind {
    pub const ALL: &'static [Self] = &[
        Self::IncidentHistory,
        Self::SourceOverhead,
        Self::PageServerReads,
        Self::QueryWaitFragments,
        Self::RuntimeFragments,
        Self::MemoryClerks,
        Self::TempdbMetadataMemory,
        Self::HyperscaleLogComponents,
        Self::PerformanceCounters,
        Self::DatabaseProperties,
        Self::SuspectPages,
        Self::TableFullText,
        Self::TableTemporal,
        Self::IndexConfiguration,
        Self::IndexedViews,
        Self::InstanceResources,
        Self::ManagedInstanceResources,
        Self::Compilation,
        Self::ConfiguredLogRates,
        Self::ConnectionOutcomes,
        Self::Jobs,
        Self::Cdc,
        Self::ChangeFeed,
        Self::ColumnstoreRowgroups,
        Self::FullTextPopulations,
        Self::TableMemory,
        Self::GroupReplication,
        Self::Replication,
        Self::PoolSettings,
        Self::WaitCounters,
        Self::Galera,
        Self::BackupHistory,
        Self::RequestMemory,
        Self::TaskSpace,
        Self::CurrentRequestWork,
        Self::StatementWork,
        Self::MaintenanceProgress,
        Self::CompletedSpills,
        Self::Storage,
        Self::Blocking,
        Self::SessionCensus,
    ];

    #[cfg(feature = "collector")]
    fn read_plan(self) -> LocalCaptureReadPlan {
        use LocalCaptureReadPlan::{Activity, Blocking, QueryHistory, Snapshot};
        use culprit_core::telemetry::SnapshotDataset as Dataset;
        match self {
            Self::IncidentHistory => LocalCaptureReadPlan::IncidentHistory,
            Self::SourceOverhead => LocalCaptureReadPlan::SourceOverhead,
            Self::PageServerReads | Self::RuntimeFragments => {
                QueryHistory(culprit_collector::outbox::QueryHistoryReadKind::Runtime)
            }
            Self::QueryWaitFragments => QueryHistory(culprit_collector::outbox::QueryHistoryReadKind::Waits),
            Self::Replication => LocalCaptureReadPlan::Replication,
            Self::PoolSettings => LocalCaptureReadPlan::PoolSettings,
            Self::WaitCounters => LocalCaptureReadPlan::WaitCounters,
            Self::ManagedInstanceResources => LocalCaptureReadPlan::ManagedInstanceResources,
            Self::Blocking => Blocking,
            Self::SessionCensus => Activity,
            Self::MemoryClerks
            | Self::TempdbMetadataMemory
            | Self::DatabaseProperties
            | Self::ConfiguredLogRates
            | Self::ConnectionOutcomes
            | Self::Storage => Snapshot(&[Dataset::Operational]),
            Self::InstanceResources => Snapshot(&[Dataset::Operational, Dataset::NumaNodes, Dataset::ProcessCpu]),
            Self::PerformanceCounters => Snapshot(&[Dataset::Operational, Dataset::DatabasePerformanceCounters]),
            Self::HyperscaleLogComponents => Snapshot(&[Dataset::HyperscaleLogComponents]),
            Self::SuspectPages => Snapshot(&[Dataset::SuspectPages]),
            Self::IndexConfiguration => Snapshot(&[Dataset::Indexes, Dataset::Tables]),
            Self::IndexedViews => Snapshot(&[Dataset::Indexes]),
            Self::TableFullText | Self::TableTemporal => Snapshot(&[Dataset::Tables]),
            Self::Compilation => Snapshot(&[Dataset::QueryCompilation]),
            Self::Jobs => Snapshot(&[Dataset::Jobs]),
            Self::Cdc => Snapshot(&[Dataset::Cdc]),
            Self::ChangeFeed => Snapshot(&[Dataset::ChangeFeed]),
            Self::ColumnstoreRowgroups => Snapshot(&[Dataset::ColumnstoreRowgroups]),
            Self::FullTextPopulations => Snapshot(&[Dataset::FullTextPopulations]),
            Self::TableMemory => Snapshot(&[Dataset::TableMemory]),
            Self::GroupReplication => Snapshot(&[Dataset::GroupReplication]),
            Self::Galera => Snapshot(&[Dataset::Galera]),
            Self::BackupHistory => Snapshot(&[Dataset::BackupHistory]),
            Self::RequestMemory => Snapshot(&[Dataset::CurrentRequestMemory]),
            Self::TaskSpace => Snapshot(&[Dataset::TaskSpace]),
            Self::CurrentRequestWork => Snapshot(&[Dataset::CurrentRequestWork]),
            Self::StatementWork => Snapshot(&[Dataset::StatementCounters]),
            Self::MaintenanceProgress => Snapshot(&[Dataset::MaintenanceProgress]),
            Self::CompletedSpills => Snapshot(&[Dataset::CachedRuntime]),
        }
    }
    #[cfg(feature = "collector")]
    pub(crate) fn accepts_snapshot_dataset(self, dataset: culprit_core::telemetry::SnapshotDataset) -> bool {
        matches!(self.read_plan(),LocalCaptureReadPlan::Snapshot(datasets) if datasets.contains(&dataset))
    }
}

#[cfg(feature = "collector")]
enum LocalCaptureReadPlan {
    IncidentHistory,
    SourceOverhead,
    ManagedInstanceResources,
    Replication,
    PoolSettings,
    WaitCounters,
    Activity,
    QueryHistory(culprit_collector::outbox::QueryHistoryReadKind),
    Snapshot(&'static [culprit_core::telemetry::SnapshotDataset]),
    Blocking,
}

/// The host delegates one saved connection; the reader independently checks the original journal association.
/// The operation begins before lookup/queueing and is never replaced after work starts.
pub struct LocalCaptureRequest<'a> {
    pub connection: &'a str,
    pub observation_id: &'a str,
    pub analysis: LocalCaptureAnalysisKind,
    pub operation: OperationHandle,
}

pub type LocalCaptureTask<'a> = Pin<Box<dyn Future<Output = Result<LocalCapturedAnalysis>> + Send + 'a>>;

#[derive(Debug)]
pub enum LocalCapturedAnalysis {
    IncidentHistory(Box<LocalIncidentHistory>),
    SourceOverhead(Box<LocalSourceOverhead>),
    ManagedInstanceResources(Box<LocalManagedInstanceResources>),
    PageServerReads(LocalPageServerReads),
    QueryWaitFragments(Box<LocalQueryWaitFragments>),
    RuntimeFragments(Box<LocalRuntimeFragments>),
    MemoryClerks(LocalMemoryClerks),
    HyperscaleLogComponents(LocalHyperscaleLogComponents),
    Snapshot(LocalSnapshotAnalysis),
    Blocking(Box<LocalBlockingAnalysis>),
    SessionCensus(Box<LocalSessionCensus>),
    Replication(Box<LocalReplication>),
    PoolSettings(Box<LocalPoolSettings>),
    WaitCounters(Box<LocalWaitCounters>),
}
impl LocalCapturedAnalysis {
    pub fn kind(&self) -> LocalCaptureAnalysisKind {
        match self {
            Self::IncidentHistory(_) => LocalCaptureAnalysisKind::IncidentHistory,
            Self::SourceOverhead(_) => LocalCaptureAnalysisKind::SourceOverhead,
            Self::PageServerReads(_) => LocalCaptureAnalysisKind::PageServerReads,
            Self::QueryWaitFragments(_) => LocalCaptureAnalysisKind::QueryWaitFragments,
            Self::RuntimeFragments(_) => LocalCaptureAnalysisKind::RuntimeFragments,
            Self::MemoryClerks(_) => LocalCaptureAnalysisKind::MemoryClerks,
            Self::HyperscaleLogComponents(_) => LocalCaptureAnalysisKind::HyperscaleLogComponents,
            Self::Snapshot(analysis) => analysis.report().kind(),
            Self::Blocking(_) => LocalCaptureAnalysisKind::Blocking,
            Self::SessionCensus(_) => LocalCaptureAnalysisKind::SessionCensus,
            Self::Replication(_) => LocalCaptureAnalysisKind::Replication,
            Self::PoolSettings(_) => LocalCaptureAnalysisKind::PoolSettings,
            Self::WaitCounters(_) => LocalCaptureAnalysisKind::WaitCounters,
            Self::ManagedInstanceResources(_) => LocalCaptureAnalysisKind::ManagedInstanceResources,
        }
    }
}

/// The default host has no local-capture capability. Installing this interface grants no native source rights.
pub trait LocalCaptureReader: Send + Sync {
    fn timeout(&self) -> Duration;
    fn analyze<'a>(&'a self, request: LocalCaptureRequest<'a>) -> LocalCaptureTask<'a>;
}

/// Fresh reanalysis of canonical local originals, never deserialized from a report cache or a warehouse wrapper.
/// Private construction keeps the raw read/projection boundary inside the original-capture owner.
#[derive(Debug)]
pub struct LocalPageServerReads {
    report: Box<PageServerReadReport>,
    history_from: String,
    history_until: String,
    population_membership: Option<RuntimePopulationMembership>,
    collected_from: String,
    collected_until: String,
    bytes_read: u64,
}
impl LocalPageServerReads {
    pub fn report(&self) -> &PageServerReadReport {
        &self.report
    }
    /// The original native history window, not elapsed collection or verification work.
    pub fn history_from(&self) -> &str {
        &self.history_from
    }
    pub fn history_until(&self) -> &str {
        &self.history_until
    }
    /// Original source semantics only; absent metadata establishes no complete native catalog.
    pub fn population_membership(&self) -> Option<&RuntimePopulationMembership> {
        self.population_membership.as_ref()
    }
    pub fn collected_from(&self) -> &str {
        &self.collected_from
    }
    pub fn collected_until(&self) -> &str {
        &self.collected_until
    }
    /// Local manifest/artifact bytes actually verified. This is neither native I/O nor source CPU work.
    pub fn bytes_read(&self) -> u64 {
        self.bytes_read
    }
}

/// Original Operational rows and their fresh interpretation share one authenticated local read.
/// Shared-instance data stays independent of a selected database or its current native member.
#[derive(Debug)]
pub struct LocalMemoryClerks {
    original: LocalOperationalCapture,
    report: Box<MemoryClerkReport>,
}
impl LocalMemoryClerks {
    pub fn original(&self) -> &LocalOperationalCapture {
        &self.original
    }
    pub fn into_read(self) -> Box<CapturedOperationalRead> {
        self.original.into_read()
    }
    pub fn report(&self) -> &MemoryClerkReport {
        &self.report
    }
}

/// One original bounded component capture; no current route or native source is acquired by this value.
#[derive(Debug)]
pub struct LocalHyperscaleLogComponents {
    original: LocalOperationalCapture,
    report: Box<culprit_core::HyperscaleLogComponentReport>,
}
impl LocalHyperscaleLogComponents {
    pub fn original(&self) -> &LocalOperationalCapture {
        &self.original
    }
    pub fn into_read(self) -> Box<CapturedOperationalRead> {
        self.original.into_read()
    }
    pub fn report(&self) -> &culprit_core::HyperscaleLogComponentReport {
        &self.report
    }
}

mod managed_instance_resources;
pub use managed_instance_resources::LocalManagedInstanceResources;

/// Exact original wait history, independent of later live database/member/interval metadata.
#[derive(Debug)]
pub struct LocalQueryWaitFragments {
    connection: String,
    read: Box<culprit_core::telemetry::CapturedQueryHistoryRead>,
    report: Box<culprit_core::query_wait_report::QueryWaitFragmentReport>,
    bytes_read: u64,
}
impl LocalQueryWaitFragments {
    pub fn connection(&self) -> &str {
        &self.connection
    }
    pub fn read(&self) -> &culprit_core::telemetry::CapturedQueryHistoryRead {
        &self.read
    }
    pub fn report(&self) -> &culprit_core::query_wait_report::QueryWaitFragmentReport {
        &self.report
    }
    pub fn bytes_read(&self) -> u64 {
        self.bytes_read
    }
    pub(crate) fn into_read(self) -> Box<culprit_core::telemetry::CapturedQueryHistoryRead> {
        self.read
    }
}

#[derive(Debug)]
pub struct LocalRuntimeFragments {
    connection: String,
    read: Box<culprit_core::telemetry::CapturedQueryHistoryRead>,
    report: Box<culprit_core::query_runtime_report::QueryRuntimeFragmentReport>,
    bytes_read: u64,
}
impl LocalRuntimeFragments {
    pub fn connection(&self) -> &str {
        &self.connection
    }
    pub fn read(&self) -> &culprit_core::telemetry::CapturedQueryHistoryRead {
        &self.read
    }
    pub fn report(&self) -> &culprit_core::query_runtime_report::QueryRuntimeFragmentReport {
        &self.report
    }
    pub fn bytes_read(&self) -> u64 {
        self.bytes_read
    }
    pub(crate) fn into_read(self) -> Box<culprit_core::telemetry::CapturedQueryHistoryRead> {
        self.read
    }
}
