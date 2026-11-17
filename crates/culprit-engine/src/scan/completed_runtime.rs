//! An explicit local capture attachment reuses the ordinary scan owner and performs no additional native read.
use crate::{PopulationReports, Workspace};
use culprit_collector::{
    capture::CollectionCancellation,
    outbox::{CompletedSnapshotBudget, OutboxDelivery},
};
use culprit_core::{
    Report, Result, SnapshotOptions,
    telemetry::{CachedRuntimeRead, CollectionStop, DiagnosticAttemptReceipt, SnapshotDataset},
};

/// One exact local artifact read, independent of native scan budgets and source cost approval.
/// Cancellation governs this local read. Once scanning starts, the existing native scan owner and its
/// cancellation/shutdown contract remain unchanged; this input creates no second controller.
pub struct CompletedRuntimeScanInput<'a> {
    pub delivery: &'a OutboxDelivery,
    pub attempt: &'a DiagnosticAttemptReceipt,
    pub budget: CompletedSnapshotBudget,
    pub cancellation: &'a CollectionCancellation,
}
impl CompletedRuntimeScanInput<'_> {
    async fn read(self) -> Result<Box<CachedRuntimeRead>> {
        let captured = self
            .delivery
            .completed_snapshot(self.attempt, SnapshotDataset::CachedRuntime, self.budget, self.cancellation)
            .await?;
        let read = captured.into_cached_runtime_read()?;
        if self.cancellation.is_cancelled() {
            return Err(CollectionStop::Cancelled.error());
        }
        Ok(Box::new(read))
    }
}

impl Workspace {
    /// Authenticate an exact completed local capture before opening the ordinary scan source. The original cache
    /// population remains independent of workload execution groups; only the requested report receives its rows.
    /// This action neither dispatches a source recipe nor grants native cost qualification.
    pub async fn scan_with_completed_runtime(
        &self,
        name: &str,
        options: &SnapshotOptions,
        input: CompletedRuntimeScanInput<'_>,
    ) -> Result<Report> {
        self.scan_populations_with_completed_runtime(name, options, input).await.map(|reports| *reports.requested)
    }

    /// The normal one-acquisition population scan, with independently captured originals attached once after
    /// confirmed native shutdown and before analysis/persistence. A conflicting requested input is refused.
    pub async fn scan_populations_with_completed_runtime(
        &self,
        name: &str,
        options: &SnapshotOptions,
        input: CompletedRuntimeScanInput<'_>,
    ) -> Result<PopulationReports> {
        let read = input.read().await?;
        Box::pin(self.scan_observation(name, options, None, Some(read))).await
    }
}
