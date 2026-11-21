//! An explicit local capture attachment reuses the ordinary scan owner and performs no additional native read.
use crate::{PopulationReports, Workspace};
use culprit_collector::{
    capture::CollectionCancellation,
    outbox::{CompletedCaptureBudget, OutboxDelivery},
};
use culprit_core::{
    Report, Result, SnapshotOptions,
    telemetry::{CollectionStop, DiagnosticAttemptReceipt, SnapshotDataset},
};

/// The retained family to attach; this selects no native source recipe or current execution population.
#[derive(Clone, Copy, Debug)]
pub enum CompletedCaptureAnalysis {
    CachedRuntime,
    MemoryClerks,
    DatabasePerformanceCounters,
    TableMemory,
    GroupReplication,
    FullTextPopulations,
    HyperscaleLogComponents,
}
impl CompletedCaptureAnalysis {
    fn dataset(self) -> SnapshotDataset {
        match self {
            Self::CachedRuntime => SnapshotDataset::CachedRuntime,
            Self::MemoryClerks => SnapshotDataset::Operational,
            Self::DatabasePerformanceCounters => SnapshotDataset::DatabasePerformanceCounters,
            Self::TableMemory => SnapshotDataset::TableMemory,
            Self::GroupReplication => SnapshotDataset::GroupReplication,
            Self::FullTextPopulations => SnapshotDataset::FullTextPopulations,
            Self::HyperscaleLogComponents => SnapshotDataset::HyperscaleLogComponents,
        }
    }
}

/// One exact local artifact read, independent of native scan budgets and source cost approval.
/// Cancellation governs this local read. Once scanning starts, the existing native scan owner and its
/// cancellation/shutdown contract remain unchanged; this input creates no second controller.
pub struct CompletedCaptureScanInput<'a> {
    pub delivery: &'a OutboxDelivery,
    pub analysis: CompletedCaptureAnalysis,
    pub attempt: &'a DiagnosticAttemptReceipt,
    pub budget: CompletedCaptureBudget,
    pub cancellation: &'a CollectionCancellation,
}
impl CompletedCaptureScanInput<'_> {
    async fn read(self, connection: &str) -> Result<super::CapturedScanEvidence> {
        let captured = self
            .delivery
            .completed_snapshot_for_connection(
                connection,
                self.attempt,
                self.analysis.dataset(),
                self.budget,
                self.cancellation,
            )
            .await?;
        let read = match self.analysis {
            CompletedCaptureAnalysis::CachedRuntime => {
                super::CapturedScanEvidence::CachedRuntime(Box::new(captured.into_cached_runtime_read()?))
            }
            CompletedCaptureAnalysis::HyperscaleLogComponents => {
                super::CapturedScanEvidence::hyperscale_log_components(Box::new(captured.into_operational_read()?))?
            }
            CompletedCaptureAnalysis::DatabasePerformanceCounters => {
                super::CapturedScanEvidence::database_performance_counters(Box::new(captured.into_operational_read()?))?
            }
            CompletedCaptureAnalysis::GroupReplication => {
                super::CapturedScanEvidence::group_replication(Box::new(captured.into_operational_read()?))?
            }
            CompletedCaptureAnalysis::TableMemory => {
                super::CapturedScanEvidence::table_memory(Box::new(captured.into_operational_read()?))?
            }
            CompletedCaptureAnalysis::FullTextPopulations => {
                super::CapturedScanEvidence::full_text_populations(Box::new(captured.into_operational_read()?))?
            }
            CompletedCaptureAnalysis::MemoryClerks => {
                super::CapturedScanEvidence::memory_clerks(Box::new(captured.into_operational_read()?))?
            }
        };
        if self.cancellation.is_cancelled() {
            return Err(CollectionStop::Cancelled.error());
        }
        Ok(read)
    }
}

impl Workspace {
    /// Authenticate an exact completed local capture before opening the ordinary scan source. Original native scope
    /// remains independent of workload execution groups; only the requested report receives its rows.
    /// This action neither dispatches a source recipe nor grants native cost qualification.
    pub async fn scan_with_completed_capture(
        &self,
        name: &str,
        options: &SnapshotOptions,
        input: CompletedCaptureScanInput<'_>,
    ) -> Result<Report> {
        self.scan_populations_with_completed_capture(name, options, input).await.map(|reports| *reports.requested)
    }

    /// The normal one-acquisition population scan, with independently captured originals attached once after
    /// confirmed native shutdown and before analysis/persistence. A conflicting requested input is refused.
    pub async fn scan_populations_with_completed_capture(
        &self,
        name: &str,
        options: &SnapshotOptions,
        input: CompletedCaptureScanInput<'_>,
    ) -> Result<PopulationReports> {
        let read = input.read(name).await?;
        Box::pin(self.scan_observation(name, options, None, Some(read))).await
    }
}
