//! One bounded state transition per call; every non-idempotent intent is persisted before its side effect.
use super::{
    committed_cache::CommittedRangeCache,
    journal::{DeliveryJournal, PendingRange},
    recovery::{CommittedRange, Fence, RecoveryStore},
    sink::*,
    verification::{PageVerification, VerificationProgress},
};
use culprit_core::{
    Result,
    telemetry::{DeliveryEntry, DeliveryPhase, DeliveryProblem},
};
use std::time::Duration;

#[derive(Clone, Copy)]
pub struct DeliveryPolicy {
    pub visibility_delay_ms: u32,
    /// Keep polling for accepted data before repeating a physical submission. None uses the poll interval.
    pub visibility_timeout_ms: Option<u32>,
    pub retry_delay_ms: u32,
    pub request_timeout_ms: u32,
    pub max_attempts: u32,
    pub verification: VerificationBudget,
}
impl Default for DeliveryPolicy {
    fn default() -> Self {
        Self {
            visibility_delay_ms: 60_000,
            visibility_timeout_ms: None,
            retry_delay_ms: 5_000,
            // One payload may require a bounded download, projection and upload. This delivery-only budget
            // is independent of the short source SQL deadlines; progress is saved after each batch/page.
            request_timeout_ms: 300_000,
            max_attempts: 10_000,
            verification: VerificationBudget::default(),
        }
    }
}
impl DeliveryPolicy {
    pub(super) fn validate(&self) -> Result<()> {
        self.verification.validate()?;
        if self.visibility_delay_ms == 0
            || self.visibility_delay_ms > 3_600_000
            || self
                .visibility_timeout_ms
                .is_some_and(|timeout| timeout < self.visibility_delay_ms || timeout > 3_600_000)
            || self.retry_delay_ms == 0
            || self.request_timeout_ms == 0
            || self.request_timeout_ms > 900_000
            || self.max_attempts == 0
        {
            return Err(DeliveryProblem::InvalidContract.error());
        }
        Ok(())
    }
}
pub struct DeliveryDriver<'a> {
    journal: &'a mut DeliveryJournal,
    store: &'a dyn RecoveryStore,
    sink: &'a dyn TelemetrySink,
    committed: &'a mut CommittedRangeCache,
    policy: DeliveryPolicy,
}
impl<'a> DeliveryDriver<'a> {
    pub fn new(
        journal: &'a mut DeliveryJournal,
        store: &'a dyn RecoveryStore,
        sink: &'a dyn TelemetrySink,
        committed: &'a mut CommittedRangeCache,
        policy: DeliveryPolicy,
    ) -> Result<Self> {
        policy.validate()?;
        Ok(Self { journal, store, sink, committed, policy })
    }
    pub async fn step(&mut self, id: &str, fence: &Fence, now_ms: i64) -> Result<DeliveryEntry> {
        self.journal.bind_store(self.store.id()).await?;
        let mut entry = self.journal.load(id).await?;
        if entry.state.retry_at_ms > now_ms && !entry.state.reconcile {
            return Ok(entry.status());
        }
        let operation = tokio::time::timeout(
            Duration::from_millis(u64::from(self.policy.request_timeout_ms)),
            self.advance(&mut entry, fence, now_ms),
        )
        .await;
        super::operation::finish(self.journal, id, entry, operation, now_ms, self.policy).await
    }

    async fn advance(&mut self, entry: &mut PendingRange, fence: &Fence, now: i64) -> Result<()> {
        let started = tokio::time::Instant::now();
        match entry.state.phase {
            DeliveryPhase::Captured => {
                super::archive_driver::advance(self.journal, self.store, entry, fence, now).await?
            }
            DeliveryPhase::Committed => {
                self.sink.validate_contract(&entry.manifest.contract).await?;
                self.attempt(entry, DeliveryPhase::DispatchPending, now).await?;
            }
            DeliveryPhase::DispatchPending => {
                let committed = self.committed(entry).await?;
                let attempt = entry.state.attempt.as_ref().ok_or_else(|| DeliveryProblem::Integrity.error())?;
                let batch = entry
                    .manifest
                    .batches
                    .iter()
                    .filter(|batch| batch.delivery_required)
                    .nth(entry.state.dispatched_batches)
                    .ok_or_else(|| DeliveryProblem::Integrity.error())?;
                match self.sink.dispatch(&committed, attempt, &batch.batch_id).await? {
                    Submission::Accepted | Submission::Unknown => {
                        entry.state.dispatched_batches += 1;
                        if entry.state.dispatched_batches
                            < entry.manifest.batches.iter().filter(|batch| batch.delivery_required).count()
                        {
                            self.journal.save(entry).await?;
                            return Ok(());
                        }
                        self.begin_visibility_wait(entry, DeliveryPhase::AwaitingData, completed_at(now, started))
                            .await?
                    }
                    Submission::Rejected => return Err(DeliveryProblem::Integrity.error()),
                }
            }
            DeliveryPhase::AwaitingData => {
                let committed = self.committed(entry).await?;
                match self.verify_page(entry, &committed, now).await {
                    Ok(Some(_)) => self.attempt(entry, DeliveryPhase::PublicationPending, now).await?,
                    Ok(None) => (),
                    Err(error)
                        if error.detail == Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::NotReady)) =>
                    {
                        self.retry_visibility(entry, DeliveryPhase::DispatchPending, now).await?
                    }
                    Err(error) => return Err(error),
                }
            }
            DeliveryPhase::PublicationPending => {
                let committed = self.committed(entry).await?;
                let publication =
                    Publication { range_id: entry.id().into(), manifest_digest: committed.manifest_digest().into() };
                if entry.state.verification.is_none() && self.sink.publication_visible(&publication, &committed).await?
                {
                    self.transition(entry, DeliveryPhase::Published, now).await?;
                } else {
                    match self.verify_page(entry, &committed, now).await {
                        Ok(Some(verified)) => {
                            let attempt =
                                entry.state.attempt.as_ref().ok_or_else(|| DeliveryProblem::Integrity.error())?;
                            let result = self.sink.publish(&verified, attempt).await?;
                            if result != publication {
                                return Err(DeliveryProblem::Integrity.error());
                            }
                            self.begin_visibility_wait(
                                entry,
                                DeliveryPhase::AwaitingPublication,
                                completed_at(now, started),
                            )
                            .await?;
                        }
                        Ok(None) => (),
                        Err(error)
                            if error.detail == Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::NotReady)) =>
                        {
                            self.attempt(entry, DeliveryPhase::DispatchPending, now).await?
                        }
                        Err(error) => return Err(error),
                    }
                }
            }
            DeliveryPhase::AwaitingPublication => {
                let committed = self.committed(entry).await?;
                let publication =
                    Publication { range_id: entry.id().into(), manifest_digest: committed.manifest_digest().into() };
                if self.sink.publication_visible(&publication, &committed).await? {
                    self.transition(entry, DeliveryPhase::Published, now).await?;
                } else {
                    self.retry_visibility(entry, DeliveryPhase::PublicationPending, now).await?;
                }
            }
            DeliveryPhase::Published => {
                let committed = self.committed(entry).await?;
                let publication = read_publication(self.sink, &committed).await?;
                self.store.retire(fence, &publication).await?;
                self.transition(entry, DeliveryPhase::Retired, now).await?;
            }
            DeliveryPhase::Retired | DeliveryPhase::Quarantined => {}
        }
        Ok(())
    }
    async fn committed(&mut self, entry: &PendingRange) -> Result<CommittedRange> {
        self.committed.get(self.store, entry.id(), &entry.manifest_digest, entry.manifest_bytes).await
    }
    async fn verify_page(
        &self,
        entry: &mut PendingRange,
        range: &CommittedRange,
        now: i64,
    ) -> Result<Option<VerifiedRange>> {
        if entry.state.verification.as_ref().is_none_or(|progress| !progress.uses(self.policy.verification)) {
            entry.state.verification = Some(VerificationProgress::new(range, self.policy.verification)?);
        }
        let progress = entry.state.verification.as_mut().ok_or_else(|| DeliveryProblem::Integrity.error())?;
        match progress.step(self.sink, range).await? {
            PageVerification::Pending => {
                entry.state.retry_at_ms = now;
                entry.state.problem = None;
                entry.state.reconcile = false;
                self.journal.save(entry).await?;
                Ok(None)
            }
            PageVerification::Complete(verified) => {
                entry.state.verification = None;
                Ok(Some(verified))
            }
        }
    }
    async fn transition(&mut self, entry: &mut PendingRange, phase: DeliveryPhase, retry_at: i64) -> Result<()> {
        entry.state.verification = None;
        entry.state.phase = phase;
        entry.state.retry_at_ms = retry_at;
        entry.state.reconcile = false;
        entry.state.problem = None;
        if !matches!(phase, DeliveryPhase::AwaitingData | DeliveryPhase::AwaitingPublication) {
            entry.state.visibility_deadline_ms = None;
        }
        self.journal.save(entry).await
    }
    async fn begin_visibility_wait(&mut self, entry: &mut PendingRange, phase: DeliveryPhase, now: i64) -> Result<()> {
        let timeout = self.policy.visibility_timeout_ms.unwrap_or(self.policy.visibility_delay_ms);
        entry.state.visibility_deadline_ms = Some(now.saturating_add(i64::from(timeout)));
        self.transition(entry, phase, now.saturating_add(i64::from(self.policy.visibility_delay_ms))).await
    }
    async fn retry_visibility(&mut self, entry: &mut PendingRange, resend: DeliveryPhase, now: i64) -> Result<()> {
        let deadline = entry.state.visibility_deadline_ms.ok_or_else(|| DeliveryProblem::Integrity.error())?;
        if now >= deadline {
            return self.attempt(entry, resend, now).await;
        }
        let next = now.saturating_add(i64::from(self.policy.visibility_delay_ms)).min(deadline);
        let phase = entry.state.phase;
        self.transition(entry, phase, next).await
    }
    async fn attempt(&mut self, entry: &mut PendingRange, phase: DeliveryPhase, now: i64) -> Result<()> {
        if phase == DeliveryPhase::DispatchPending {
            entry.state.dispatched_batches = 0;
        }
        let ordinal = entry.state.attempt.as_ref().map_or(1, |attempt| attempt.ordinal.saturating_add(1));
        if ordinal > self.policy.max_attempts {
            entry.state.phase = DeliveryPhase::Quarantined;
            return Err(DeliveryProblem::Capacity.error());
        }
        entry.state.attempt = Some(DeliveryAttempt::new(ordinal)?);
        self.transition(entry, phase, now).await
    }
    pub async fn poll_updates(&mut self) -> Result<()> {
        self.journal.bind_store(self.store.id()).await?;
        let cursor = self.journal.poll_cursor().await?;
        let updates = tokio::time::timeout(
            Duration::from_millis(u64::from(self.policy.request_timeout_ms)),
            self.sink.poll_updates(cursor.as_deref(), 128),
        )
        .await
        .map_err(|_| DeliveryProblem::Transport.error())??;
        self.journal.apply_updates(updates).await?;
        self.journal.reconcile_expired(128).await?;
        Ok(())
    }
}
fn completed_at(now: i64, started: tokio::time::Instant) -> i64 {
    now.saturating_add(started.elapsed().as_millis().min(i64::MAX as u128) as i64)
}
