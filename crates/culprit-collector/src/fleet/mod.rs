//! Destination-independent static fleet collection. The embedding backend retains authentication and native guards.
mod admission;
mod history;
mod investigations;
mod journal;
mod scheduler;
mod storage;
mod windows;
mod work;
pub use journal::FleetJournal;

use crate::{
    capture::{CollectionAdmissionWindow, CollectionCancellation},
    outbox::Outbox,
};
use async_trait::async_trait;
use culprit_core::{Result, telemetry::*};
use scheduler::Scheduler;
use std::time::Duration;
use storage::CaptureStorage;

/// The selected operation retains its persisted window and, when available, a frozen saved-route revision.
#[derive(Clone, Debug)]
pub struct FleetSourceAdmission {
    pub window: CollectionAdmissionWindow,
    pub route_revision: Option<String>,
    pub investigation: Option<DiagnosticInvestigationIntent>,
}

/// The capture owner returns native closure independently of success, storage failures or translated errors.
pub struct FleetCaptureOutcome {
    pub result: Result<CollectionManifest>,
    pub source_work: SourceWorkState,
}

#[async_trait]
pub trait FleetBackend: Send + Sync {
    /// Validate saved metadata without opening a native connection. No interactive authentication fallback is allowed.
    fn validate_target(&self, connection: &str) -> Result<()>;
    /// Read the selected provider's declarations without credentials, provider construction or native work.
    /// An empty catalog is explicitly undeclared; it never inherits another provider's native recipes.
    fn telemetry_recipes(&self, _connection: &str) -> Result<Vec<RecipeDescriptor>> {
        Ok(Vec::new())
    }
    /// Fingerprint saved source-routing metadata only; never resolve credentials or open a provider.
    fn route_revision(&self, _connection: &str) -> Result<Option<String>> {
        Ok(None)
    }
    /// Identify the selected source's qualification authority without resolving credentials or opening it.
    fn qualification_component(&self, _connection: &str) -> Result<Option<culprit_core::ComponentVersion>> {
        Ok(None)
    }
    /// Interpret an independently authenticated cost artifact. A structural receipt alone grants no permission.
    fn verify_cost(
        &self,
        _connection: &str,
        _receipt: &RecipeCostReceipt,
        _source: &CollectionDescription,
        _record: &ExportedRecord,
    ) -> Result<bool> {
        Ok(false)
    }
    /// Own the entire native lifecycle and report cancellation uncertainty through CollectionStop.
    async fn discover(
        &self,
        connection: &str,
        request: &IntervalDiscoveryRequest,
        admission: FleetSourceAdmission,
    ) -> Result<IntervalDiscoveryPage>;
    /// Reuse the same source admission, authentication, cancellation and outbox path as interactive collection.
    async fn capture(
        &self,
        connection: &str,
        request: CollectionRequest,
        outbox: &mut Outbox,
        cancellation: &CollectionCancellation,
        admission: FleetSourceAdmission,
    ) -> FleetCaptureOutcome;
}

pub struct FleetRunner<'a> {
    backend: &'a dyn FleetBackend,
    journal: &'a FleetJournal,
    storage: CaptureStorage<'a>,
    scheduler: Scheduler,
    started: tokio::time::Instant,
    initialized: bool,
    stopped: bool,
    diagnostics: crate::diagnostics::DiagnosticPolicies,
    investigations: investigations::Investigations,
}
impl<'a> FleetRunner<'a> {
    pub fn new(
        backend: &'a dyn FleetBackend,
        inventory: &FleetInventory,
        shard: &str,
        journal: &'a FleetJournal,
        outbox: &'a mut Outbox,
    ) -> Result<Self> {
        let scheduler = Scheduler::new(inventory, shard)?;
        journal.validate_assignment(inventory, shard)?;
        for target in inventory.targets.iter().filter(|target| target.shard == shard) {
            backend.validate_target(&target.connection)?;
        }
        let mut diagnostics = crate::diagnostics::DiagnosticPolicies::from_inventory(inventory, shard)?;
        let connections = diagnostics.connections().into_iter().map(str::to_owned).collect::<Vec<_>>();
        for connection in connections {
            diagnostics = diagnostics.with_recipes(&connection, backend.telemetry_recipes(&connection)?)?;
        }
        Ok(Self {
            backend,
            journal,
            storage: CaptureStorage::local(outbox),
            scheduler,
            started: tokio::time::Instant::now(),
            initialized: false,
            stopped: false,
            diagnostics,
            investigations: investigations::Investigations::new(inventory, shard),
        })
    }
    pub fn with_recovery(mut self, recovery: &'a dyn crate::delivery::recovery::RecoveryStore) -> Result<Self> {
        self.storage.recovery = Some(recovery);
        self.storage.validate()?;
        Ok(self)
    }
    pub async fn run(
        &mut self,
        cancellation: &CollectionCancellation,
        max_work: Option<u32>,
        report: impl FnMut(&FleetCollectionEvent) -> Result<()>,
    ) -> Result<FleetSummary> {
        self.storage.validate()?;
        if max_work == Some(0) {
            return Err(scheduler::state_failed());
        }
        if self.stopped {
            return Err(scheduler::state_failed());
        }
        let result = self.run_ready(cancellation, max_work, report).await;
        if result.is_err() {
            self.stopped = true;
        }
        result
    }
    async fn initialize(&mut self, cancellation: &CollectionCancellation) -> Result<()> {
        if self.initialized {
            return Ok(());
        }
        let now = self.started.elapsed();
        let wall = chrono::Utc::now();
        match self.journal.scheduler().await? {
            Some(state) => {
                self.scheduler.restore(state, now, wall)?;
                if let Some(checkpoint) = self.scheduler.resumed_admission_checkpoint(now, wall)? {
                    let clock = self.journal.update_scheduler(checkpoint).await?;
                    self.scheduler.persisted(clock)?;
                }
            }
            None => {
                let state = self.scheduler.initial_state(now, wall)?;
                let clock = self.journal.initialize_scheduler(state).await?;
                self.scheduler.persisted(clock)?;
            }
        }
        self.investigations
            .reconcile(self.backend, self.journal, &mut self.storage, &mut self.scheduler, self.started, cancellation)
            .await?;
        self.initialized = true;
        Ok(())
    }
    async fn save_admission(&mut self, admitted_at: Duration) -> Result<()> {
        let checkpoint = self.scheduler.admission_checkpoint(admitted_at, chrono::Utc::now())?;
        let clock = self.journal.update_scheduler(checkpoint).await?;
        self.scheduler.persisted(clock)
    }
    async fn save_completion(&mut self) -> Result<()> {
        let checkpoint = self.scheduler.completion_checkpoint(self.started.elapsed(), chrono::Utc::now())?;
        let clock = self.journal.update_scheduler(checkpoint).await?;
        self.scheduler.persisted(clock)
    }
    async fn run_ready(
        &mut self,
        cancellation: &CollectionCancellation,
        max_work: Option<u32>,
        mut report: impl FnMut(&FleetCollectionEvent) -> Result<()>,
    ) -> Result<FleetSummary> {
        let mut summary = FleetSummary::default();
        if cancellation.is_cancelled() {
            return Ok(summary);
        }
        self.initialize(cancellation).await?;
        while !cancellation.is_cancelled() && max_work.is_none_or(|limit| summary.attempts < limit) {
            self.analyze_pending(cancellation).await?;
            if cancellation.is_cancelled() {
                break;
            }
            let Some((job, admitted_at)) = self.next_work(cancellation).await? else {
                wait(cancellation, self.next_delay()).await;
                continue;
            };
            self.save_admission(admitted_at).await?;
            if cancellation.is_cancelled() {
                let outcome = match &job {
                    scheduler::Work::Scheduled(_) => scheduler::Outcome::Failed,
                    scheduler::Work::Focused(_) => scheduler::Outcome::FocusedRetry {
                        reason: scheduler::FocusRetry::Failed,
                        attempt: None,
                        source_work: SourceWorkState::NotStarted,
                    },
                };
                self.scheduler.finish(self.started.elapsed(), outcome)?;
                self.save_completion().await?;
                break;
            }
            let maintain_investigations = matches!(&job, scheduler::Work::Focused(_))
                || matches!(&job, scheduler::Work::Scheduled(job)
                    if matches!(job.schedule.collection, ScheduledCollection::Activity { .. }));
            let result = self.execute_work(&job, admitted_at, cancellation).await?;
            self.scheduler.finish(self.started.elapsed(), result.outcome)?;
            self.save_completion().await?;
            self.analyze_pending(cancellation).await?;
            if maintain_investigations {
                self.investigations
                    .reconcile(
                        self.backend,
                        self.journal,
                        &mut self.storage,
                        &mut self.scheduler,
                        self.started,
                        cancellation,
                    )
                    .await?;
            }
            summary.attempts = summary.attempts.checked_add(1).ok_or_else(scheduler::state_failed)?;
            if result.event.status == CollectionStatus::Complete {
                summary.complete += 1;
            } else {
                summary.incomplete += 1;
            }
            report(&result.event)?;
            if result.pause_all {
                // Delivery uses separate dependencies and remains runnable while source capacity is exhausted.
                wait(cancellation, Duration::from_secs(60)).await;
            }
        }
        Ok(summary)
    }
    fn next_delay(&self) -> Duration {
        self.scheduler.wait_allowed(self.started.elapsed(), |job| self.investigations.route_available(job))
    }

    async fn next_work(
        &mut self,
        cancellation: &CollectionCancellation,
    ) -> Result<Option<(scheduler::Work, Duration)>> {
        let mut admitted_at = self.started.elapsed();
        let mut next = self.scheduler.take_allowed(admitted_at, |job| self.investigations.route_available(job))?;
        if next.is_none() && self.next_delay() >= self.investigations.planning_time() {
            self.investigations
                .prepare(self.backend, self.journal, &mut self.storage, &mut self.scheduler, self.started, cancellation)
                .await?;
            admitted_at = self.started.elapsed();
            next = self.scheduler.take_allowed(admitted_at, |job| self.investigations.route_available(job))?;
        }
        Ok(next.map(|job| (job, admitted_at)))
    }

    async fn execute_work(
        &mut self,
        job: &scheduler::Work,
        admitted_at: Duration,
        cancellation: &CollectionCancellation,
    ) -> Result<work::WorkResult> {
        let backend = admission::AdmittedBackend::new(self.backend, self.started + admitted_at);
        // Discovery retains native ownership until its bounded completion; capture has its own cancellation owner.
        match job {
            scheduler::Work::Scheduled(job) => {
                self.investigations.starting_baseline(job);
                let result =
                    work::execute(&backend, self.journal, &mut self.storage, job, cancellation, chrono::Utc::now())
                        .await?;
                self.investigations.completed_baseline(job, &result, backend.used_route_revision())?;
                Ok(result)
            }
            scheduler::Work::Focused(job) => {
                self.investigations
                    .execute(self.backend, &backend, self.journal, &mut self.storage, job, cancellation)
                    .await
            }
        }
    }
    async fn analyze_pending(&mut self, cancellation: &CollectionCancellation) -> Result<()> {
        crate::diagnostics::analyze_pending(
            self.storage.outbox,
            &self.diagnostics,
            cancellation,
            MAX_DIAGNOSTIC_INPUTS as u32,
            self.storage.recovery,
        )
        .await?;
        Ok(())
    }
}
async fn wait(cancellation: &CollectionCancellation, delay: Duration) {
    tokio::select! {biased; _=cancellation.cancelled()=>{}, _=tokio::time::sleep(delay)=>{}}
}

#[cfg(test)]
mod tests;
