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
mod session_census;
pub use session_census::LocalSessionCensus;

/// A closed analysis choice; selecting an analysis starts no native collection or source discovery.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum LocalCaptureAnalysisKind {
    PageServerReads,
    MemoryClerks,
    TempdbMetadataMemory,
    HyperscaleLogComponents,
    PerformanceCounters,
    DatabaseProperties,
    TableFullText,
    InstanceResources,
    Compilation,
    ConfiguredLogRates,
    ConnectionOutcomes,
    Jobs,
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
    pub const ALL: [Self; 21] = [
        Self::PageServerReads,
        Self::MemoryClerks,
        Self::TempdbMetadataMemory,
        Self::HyperscaleLogComponents,
        Self::PerformanceCounters,
        Self::DatabaseProperties,
        Self::TableFullText,
        Self::InstanceResources,
        Self::Compilation,
        Self::ConfiguredLogRates,
        Self::ConnectionOutcomes,
        Self::Jobs,
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
            Self::PageServerReads => QueryHistory,
            Self::Blocking => Blocking,
            Self::SessionCensus => Activity,
            Self::MemoryClerks
            | Self::TempdbMetadataMemory
            | Self::PerformanceCounters
            | Self::InstanceResources
            | Self::DatabaseProperties
            | Self::ConfiguredLogRates
            | Self::ConnectionOutcomes
            | Self::Storage => Snapshot(Dataset::Operational),
            Self::HyperscaleLogComponents => Snapshot(Dataset::HyperscaleLogComponents),
            Self::TableFullText => Snapshot(Dataset::Tables),
            Self::Compilation => Snapshot(Dataset::QueryCompilation),
            Self::Jobs => Snapshot(Dataset::Jobs),
            Self::RequestMemory => Snapshot(Dataset::CurrentRequestMemory),
            Self::TaskSpace => Snapshot(Dataset::TaskSpace),
            Self::CurrentRequestWork => Snapshot(Dataset::CurrentRequestWork),
            Self::StatementWork => Snapshot(Dataset::StatementCounters),
            Self::MaintenanceProgress => Snapshot(Dataset::MaintenanceProgress),
            Self::CompletedSpills => Snapshot(Dataset::CachedRuntime),
        }
    }
    #[cfg(feature = "collector")]
    pub(crate) fn snapshot_dataset(self) -> Option<culprit_core::telemetry::SnapshotDataset> {
        match self.read_plan() {
            LocalCaptureReadPlan::Snapshot(dataset) => Some(dataset),
            _ => None,
        }
    }
}

#[cfg(feature = "collector")]
enum LocalCaptureReadPlan {
    Activity,
    QueryHistory,
    Snapshot(culprit_core::telemetry::SnapshotDataset),
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
    PageServerReads(LocalPageServerReads),
    MemoryClerks(LocalMemoryClerks),
    HyperscaleLogComponents(LocalHyperscaleLogComponents),
    Snapshot(LocalSnapshotAnalysis),
    Blocking(Box<LocalBlockingAnalysis>),
    SessionCensus(Box<LocalSessionCensus>),
}
impl LocalCapturedAnalysis {
    pub fn kind(&self) -> LocalCaptureAnalysisKind {
        match self {
            Self::PageServerReads(_) => LocalCaptureAnalysisKind::PageServerReads,
            Self::MemoryClerks(_) => LocalCaptureAnalysisKind::MemoryClerks,
            Self::HyperscaleLogComponents(_) => LocalCaptureAnalysisKind::HyperscaleLogComponents,
            Self::Snapshot(analysis) => analysis.report().kind(),
            Self::Blocking(_) => LocalCaptureAnalysisKind::Blocking,
            Self::SessionCensus(_) => LocalCaptureAnalysisKind::SessionCensus,
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
