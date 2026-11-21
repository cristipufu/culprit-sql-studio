//! An explicit local capture attachment reuses the ordinary scan owner and performs no additional native read.
use crate::{PopulationReports, Workspace};
use culprit_collector::{
    capture::CollectionCancellation,
    outbox::{CompletedCaptureBudget, OutboxDelivery},
};
use tokio::time::Instant;

use culprit_core::{
    Report, Result, SnapshotOptions,
    telemetry::{CollectionStop, DiagnosticAttemptReceipt, SnapshotDataset},
};

/// The retained family to attach; this selects no native source recipe or current execution population.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompletedCaptureAnalysis {
    ManagedInstanceResources,
    ProcessCpu,
    CachedRuntime,
    MemoryClerks,
    DatabasePerformanceCounters,
    TableMemory,
    IndexConfiguration { dataset: SnapshotDataset },
    IndexedViews,
    GroupReplication,
    Galera,
    PoolSettings,
    FullTextPopulations,
    HyperscaleLogComponents,
}
impl CompletedCaptureAnalysis {
    fn dataset(self) -> Result<SnapshotDataset> {
        Ok(match self {
            Self::ManagedInstanceResources => {
                return Err(culprit_core::telemetry::DeliveryProblem::UnsupportedSchema.error());
            }
            Self::ProcessCpu => SnapshotDataset::ProcessCpu,
            Self::CachedRuntime => SnapshotDataset::CachedRuntime,
            Self::MemoryClerks => SnapshotDataset::Operational,
            Self::DatabasePerformanceCounters => SnapshotDataset::DatabasePerformanceCounters,
            Self::IndexConfiguration { dataset }
                if matches!(dataset, SnapshotDataset::Indexes | SnapshotDataset::Tables) =>
            {
                dataset
            }
            Self::IndexConfiguration { .. } => {
                return Err(culprit_core::telemetry::DeliveryProblem::UnsupportedSchema.error());
            }
            Self::IndexedViews => SnapshotDataset::Indexes,
            Self::TableMemory => SnapshotDataset::TableMemory,
            Self::GroupReplication => SnapshotDataset::GroupReplication,
            Self::Galera => SnapshotDataset::Galera,
            Self::PoolSettings => SnapshotDataset::Settings,
            Self::FullTextPopulations => SnapshotDataset::FullTextPopulations,
            Self::HyperscaleLogComponents => SnapshotDataset::HyperscaleLogComponents,
        })
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
    async fn read(
        self,
        connection: &str,
        budget: CompletedCaptureBudget,
        deadline: Instant,
    ) -> Result<CompletedOriginal> {
        if matches!(self.analysis, CompletedCaptureAnalysis::ManagedInstanceResources) {
            let original = self
                .delivery
                .completed_managed_instance_resources_for_connection_until(
                    connection,
                    self.attempt,
                    budget,
                    self.cancellation,
                    deadline,
                )
                .await?;
            let bytes = original.bytes_read();
            let original = original.into_read()?;
            let rows = original.observations.len();
            let read = super::CapturedScanEvidence::managed_instance_resources(Box::new(original))?;
            if self.cancellation.is_cancelled() {
                return Err(CollectionStop::Cancelled.error());
            }
            return Ok(CompletedOriginal { evidence: read, rows, bytes });
        }
        let captured = self
            .delivery
            .completed_snapshot_for_connection_until(
                connection,
                self.attempt,
                self.analysis.dataset()?,
                budget,
                self.cancellation,
                deadline,
            )
            .await?;
        let rows = captured.records().len();
        let bytes = captured.bytes_read();
        let read = match self.analysis {
            CompletedCaptureAnalysis::ManagedInstanceResources => {
                return Err(culprit_core::telemetry::DeliveryProblem::UnsupportedSchema.error());
            }
            CompletedCaptureAnalysis::PoolSettings => {
                super::CapturedScanEvidence::pool_settings(Box::new(captured.into_pool_settings_read()?))?
            }
            CompletedCaptureAnalysis::ProcessCpu => {
                super::CapturedScanEvidence::process_cpu(Box::new(captured.into_operational_read()?))?
            }
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
            CompletedCaptureAnalysis::Galera => {
                super::CapturedScanEvidence::galera(Box::new(captured.into_operational_read()?))?
            }
            CompletedCaptureAnalysis::IndexedViews => {
                super::CapturedScanEvidence::indexed_views(Box::new(captured.into_operational_read()?))?
            }
            CompletedCaptureAnalysis::IndexConfiguration { .. } => {
                super::CapturedScanEvidence::index_configuration(Box::new(captured.into_operational_read()?))?
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
        Ok(CompletedOriginal { evidence: read, rows, bytes })
    }
}

struct CompletedOriginal {
    evidence: super::CapturedScanEvidence,
    rows: usize,
    bytes: u64,
}

/// One action's local verification allowance. Native acquisition keeps its existing independent owner.
struct OriginalReadAllowance {
    rows: u32,
    bytes: u64,
    deadline: Instant,
}
impl OriginalReadAllowance {
    fn new(budget: CompletedCaptureBudget, started: Instant) -> Result<Self> {
        let budget = budget.validate()?;
        let deadline = started.checked_add(budget.timeout).ok_or_else(|| CollectionStop::TimedOut.error())?;
        let allowance = Self { rows: budget.max_rows, bytes: budget.max_bytes, deadline };
        allowance.check()?;
        Ok(allowance)
    }
    fn for_input(&self, input: &CompletedCaptureScanInput<'_>) -> Result<(CompletedCaptureBudget, Instant)> {
        let input = input.budget.validate()?;
        let now = Instant::now();
        let cutoff = now.checked_add(input.timeout).ok_or_else(|| CollectionStop::TimedOut.error())?.min(self.deadline);
        let timeout = cutoff.saturating_duration_since(now);
        if timeout.is_zero() {
            return Err(CollectionStop::TimedOut.error());
        }
        let budget = CompletedCaptureBudget {
            max_rows: self.rows.min(input.max_rows),
            max_bytes: self.bytes.min(input.max_bytes),
            timeout,
        }
        .validate()?;
        Ok((budget, cutoff))
    }
    fn charge(&mut self, original: &CompletedOriginal) -> Result<()> {
        self.rows =
            self.rows.checked_sub(u32::try_from(original.rows).map_err(|_| capacity())?).ok_or_else(capacity)?;
        self.bytes = self.bytes.checked_sub(original.bytes).ok_or_else(capacity)?;
        self.check()
    }
    fn check(&self) -> Result<()> {
        if Instant::now() >= self.deadline {
            return Err(CollectionStop::TimedOut.error());
        }
        Ok(())
    }
}
fn capacity() -> culprit_core::Error {
    culprit_core::telemetry::DeliveryProblem::Capacity.error()
}

fn validate_selection(inputs: &[CompletedCaptureScanInput<'_>]) -> Result<()> {
    // Inspect every claimed original before the cap. A late conflicting copy must never leave a plausible prefix.
    let mut claimed = std::collections::BTreeMap::new();
    for input in inputs {
        // Serialize the existing complete identity type, rather than inventing a partial native/name key.
        let identity = serde_json::to_string(&input.attempt.capture)
            .map_err(|_| culprit_core::telemetry::DeliveryProblem::InvalidContract.error())?;
        if let Some(attempt) = claimed.insert(identity, input.attempt)
            && attempt != input.attempt
        {
            return Err(culprit_core::telemetry::DeliveryProblem::Conflict.error());
        }
    }
    if inputs.is_empty() || inputs.len() > culprit_core::telemetry::MAX_RECORDED_OPERATIONAL_READS {
        return Err(capacity());
    }
    for input in inputs {
        if !input.attempt.is_valid() || input.attempt.manifest_sha256.is_none() {
            return Err(culprit_core::telemetry::DeliveryProblem::InvalidContract.error());
        }
        input.budget.validate()?;
        if !matches!(input.analysis, CompletedCaptureAnalysis::ManagedInstanceResources) {
            input.analysis.dataset()?;
        }
    }
    check_cancellation(inputs)
}
fn check_cancellation(inputs: &[CompletedCaptureScanInput<'_>]) -> Result<()> {
    if inputs.iter().any(|input| input.cancellation.is_cancelled()) {
        return Err(CollectionStop::Cancelled.error());
    }
    Ok(())
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
        observation: culprit_core::ProviderOperationObservation,
    ) -> Result<culprit_core::ProviderCall<Box<Report>>> {
        self.scan_populations_with_completed_capture(name, options, input, observation)
            .await
            .map(crate::provider_reads::requested_call)
    }

    /// Singular attachment uses the same shared local-read allowance and native scan owner as plural attachment.
    pub async fn scan_populations_with_completed_capture(
        &self,
        name: &str,
        options: &SnapshotOptions,
        input: CompletedCaptureScanInput<'_>,
        observation: culprit_core::ProviderOperationObservation,
    ) -> Result<culprit_core::ProviderCall<PopulationReports>> {
        let budget = input.budget;
        self.scan_populations_with_completed_captures(name, options, vec![input], budget, observation).await
    }

    /// Attach explicitly selected originals; no latest lookup, automatic pairing or current-source rebinding.
    pub async fn scan_with_completed_captures(
        &self,
        name: &str,
        options: &SnapshotOptions,
        inputs: Vec<CompletedCaptureScanInput<'_>>,
        budget: CompletedCaptureBudget,
        observation: culprit_core::ProviderOperationObservation,
    ) -> Result<culprit_core::ProviderCall<Box<Report>>> {
        self.scan_populations_with_completed_captures(name, options, inputs, budget, observation)
            .await
            .map(crate::provider_reads::requested_call)
    }

    /// Authenticate every selected original under one absolute local deadline and total row/byte allowance before
    /// any provider acquisition. An error or cancellation returns without a native login. One ordinary scan owner
    /// settles once, then attaches all authenticated originals before analysis/persistence. Repetitions retain the
    /// shared immutable append policy; neither a second controller nor a new per-original native budget is created.
    pub async fn scan_populations_with_completed_captures(
        &self,
        name: &str,
        options: &SnapshotOptions,
        inputs: Vec<CompletedCaptureScanInput<'_>>,
        budget: CompletedCaptureBudget,
        observation: culprit_core::ProviderOperationObservation,
    ) -> Result<culprit_core::ProviderCall<PopulationReports>> {
        observation.require_kind(culprit_core::ProviderReadKind::Scan)?;
        let started = Instant::now();
        validate_selection(&inputs)?;
        let mut allowance = OriginalReadAllowance::new(budget, started)?;
        let mut originals = Vec::with_capacity(inputs.len());
        // Keep cancellation references until every original has authenticated and the final admission check passes.
        let cancellations = inputs.iter().map(|input| input.cancellation).collect::<Vec<_>>();
        for input in inputs {
            if cancellations.iter().any(|cancellation| cancellation.is_cancelled()) {
                return Err(CollectionStop::Cancelled.error());
            }
            let (budget, cutoff) = allowance.for_input(&input)?;
            let original = input.read(name, budget, cutoff).await?;
            allowance.charge(&original)?;
            originals.push(original.evidence);
        }
        allowance.check()?;
        if cancellations.iter().any(|cancellation| cancellation.is_cancelled()) {
            return Err(CollectionStop::Cancelled.error());
        }
        Box::pin(self.scan_observation(
            name,
            options,
            None,
            Some(super::CapturedScanEvidence::Multiple(originals)),
            observation,
        ))
        .await
    }
}

#[cfg(test)]
mod tests;
