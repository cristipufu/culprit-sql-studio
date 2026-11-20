//! One original Operational owner is shared by local report families and typed scan-attachment results.
use super::LocalCaptureAnalysisKind;
use culprit_core::{telemetry::CapturedOperationalRead, *};

#[derive(Debug)]
pub struct LocalOperationalCapture {
    pub(super) connection: String,
    pub(super) read: Box<CapturedOperationalRead>,
    pub(super) collected_from: String,
    pub(super) collected_until: String,
    pub(super) bytes_read: u64,
}
impl LocalOperationalCapture {
    /// Exact journal association, never a database name or endpoint inference.
    pub fn connection(&self) -> &str {
        &self.connection
    }
    pub fn read(&self) -> &CapturedOperationalRead {
        &self.read
    }
    pub fn into_read(self) -> Box<CapturedOperationalRead> {
        self.read
    }
    /// Original client acquisition clocks; native observations retain their own independent bounds.
    pub fn collected_from(&self) -> &str {
        &self.collected_from
    }
    pub fn collected_until(&self) -> &str {
        &self.collected_until
    }
    /// Verified local file bytes, not native physical I/O or CPU work.
    pub fn bytes_read(&self) -> u64 {
        self.bytes_read
    }
}

#[derive(Debug)]
pub struct LocalSnapshotAnalysis {
    pub(super) original: LocalOperationalCapture,
    pub(super) report: LocalSnapshotReport,
}
impl LocalSnapshotAnalysis {
    pub fn original(&self) -> &LocalOperationalCapture {
        &self.original
    }
    pub fn report(&self) -> &LocalSnapshotReport {
        &self.report
    }
}

/// Typed existing pure reports; none of these values creates a new native source or diagnostic authority.
#[derive(Debug)]
pub enum LocalSnapshotReport {
    PerformanceCounters(Box<PerformanceCounterReport>),
    TempdbMetadataMemory(Box<TempdbMetadataMemoryReport>),
    DatabaseProperties(Box<DatabasePropertiesReport>),
    InstanceResources(Box<InstanceResourceReport>),
    Compilation(Box<CompilationReport>),
    ConfiguredLogRates(Box<ConfiguredLogRateReport>),
    ConnectionOutcomes(Box<ConnectionOutcomeReport>),
    Jobs(Box<JobsReport>),
    RequestMemory(Box<MemoryParticipantsReport>),
    TaskSpace(Box<TaskSpaceReport>),
    CurrentRequestWork(Box<CurrentRequestWorkReport>),
    StatementWork(Box<StatementWorkReport>),
    MaintenanceProgress(Box<MaintenanceProgressReport>),
    CompletedSpills(Box<CompletedSpillReport>),
    Storage(Box<StorageReport>),
}
impl LocalSnapshotReport {
    pub fn kind(&self) -> LocalCaptureAnalysisKind {
        match self {
            Self::PerformanceCounters(_) => LocalCaptureAnalysisKind::PerformanceCounters,
            Self::TempdbMetadataMemory(_) => LocalCaptureAnalysisKind::TempdbMetadataMemory,
            Self::InstanceResources(_) => LocalCaptureAnalysisKind::InstanceResources,
            Self::DatabaseProperties(_) => LocalCaptureAnalysisKind::DatabaseProperties,
            Self::Compilation(_) => LocalCaptureAnalysisKind::Compilation,
            Self::ConfiguredLogRates(_) => LocalCaptureAnalysisKind::ConfiguredLogRates,
            Self::ConnectionOutcomes(_) => LocalCaptureAnalysisKind::ConnectionOutcomes,
            Self::Jobs(_) => LocalCaptureAnalysisKind::Jobs,
            Self::RequestMemory(_) => LocalCaptureAnalysisKind::RequestMemory,
            Self::TaskSpace(_) => LocalCaptureAnalysisKind::TaskSpace,
            Self::CurrentRequestWork(_) => LocalCaptureAnalysisKind::CurrentRequestWork,
            Self::StatementWork(_) => LocalCaptureAnalysisKind::StatementWork,
            Self::MaintenanceProgress(_) => LocalCaptureAnalysisKind::MaintenanceProgress,
            Self::CompletedSpills(_) => LocalCaptureAnalysisKind::CompletedSpills,
            Self::Storage(_) => LocalCaptureAnalysisKind::Storage,
        }
    }
}
