//! Destination-independent static fleet collection. The embedding backend retains authentication and native guards.
mod admission;
mod history;
mod journal;
mod scheduler;
mod storage;
mod windows;
mod work;
pub use journal::FleetJournal;

use crate::{capture::CollectionCancellation, outbox::Outbox};
use async_trait::async_trait;
use culprit_core::{Result, telemetry::*};
use scheduler::Scheduler;
use std::time::Duration;
use storage::CaptureStorage;

#[async_trait]
pub trait FleetBackend: Send + Sync {
    /// Validate saved metadata without opening a native connection. No interactive authentication fallback is allowed.
    fn validate_target(&self, connection: &str) -> Result<()>;
    /// Own the entire native lifecycle and report cancellation uncertainty through CollectionStop.
    async fn discover(&self, connection: &str, request: &IntervalDiscoveryRequest) -> Result<IntervalDiscoveryPage>;
    /// Reuse the same source admission, authentication, cancellation and outbox path as interactive collection.
    async fn capture(
        &self,
        connection: &str,
        request: CollectionRequest,
        outbox: &mut Outbox,
        cancellation: &CollectionCancellation,
    ) -> Result<CollectionManifest>;
}

pub struct FleetRunner<'a> {
    backend: &'a dyn FleetBackend,
    journal: &'a FleetJournal,
    storage: CaptureStorage<'a>,
    scheduler: Scheduler,
    started: tokio::time::Instant,
    initialized: bool,
    stopped: bool,
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
        Ok(Self {
            backend,
            journal,
            storage: CaptureStorage::local(outbox),
            scheduler,
            started: tokio::time::Instant::now(),
            initialized: false,
            stopped: false,
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
    async fn initialize(&mut self) -> Result<()> {
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
        self.initialize().await?;
        while !cancellation.is_cancelled() && max_work.is_none_or(|limit| summary.attempts < limit) {
            let admitted_at = self.started.elapsed();
            let Some(job) = self.scheduler.take(admitted_at)? else {
                wait(cancellation, self.scheduler.wait(self.started.elapsed())).await;
                continue;
            };
            self.save_admission(admitted_at).await?;
            if cancellation.is_cancelled() {
                self.scheduler.finish(self.started.elapsed(), scheduler::Outcome::Failed)?;
                self.save_completion().await?;
                break;
            }
            let backend = admission::AdmittedBackend::new(self.backend, self.started + admitted_at);
            // Let admitted discovery finish under its native budget; dropping it on shutdown would lose its proof.
            // Capture has its own cancellation-aware controller and also returns only after settling ownership.
            let result =
                work::execute(&backend, self.journal, &mut self.storage, &job, cancellation, chrono::Utc::now())
                    .await?;
            self.scheduler.finish(self.started.elapsed(), result.outcome)?;
            self.save_completion().await?;
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
}
async fn wait(cancellation: &CollectionCancellation, delay: Duration) {
    tokio::select! {biased; _=cancellation.cancelled()=>{}, _=tokio::time::sleep(delay)=>{}}
}

#[cfg(test)]
mod tests;
