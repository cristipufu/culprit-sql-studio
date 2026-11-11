//! Native recovery advances independently of analytical credentials, requests and local target queue capacity.
use super::{
    driver::DeliveryPolicy,
    journal::{DeliveryJournal, PendingRange},
    operation,
    recovery::{self, Fence, RecoveryStore},
};
use culprit_core::{
    Result,
    telemetry::{DeliveryEntry, DeliveryPhase, DeliveryProblem},
};
use std::time::Duration;

pub(super) struct ArchiveDriver<'a> {
    journal: &'a mut DeliveryJournal,
    store: &'a dyn RecoveryStore,
    policy: DeliveryPolicy,
}
impl<'a> ArchiveDriver<'a> {
    pub fn new(journal: &'a mut DeliveryJournal, store: &'a dyn RecoveryStore, policy: DeliveryPolicy) -> Result<Self> {
        policy.validate()?;
        Ok(Self { journal, store, policy })
    }
    pub async fn step(&mut self, id: &str, fence: &Fence, now: i64) -> Result<DeliveryEntry> {
        self.journal.bind_store(self.store.id()).await?;
        let mut entry = self.journal.load(id).await?;
        if entry.state.phase != DeliveryPhase::Captured || entry.state.retry_at_ms > now && !entry.state.reconcile {
            return Ok(entry.status());
        }
        let result = tokio::time::timeout(
            Duration::from_millis(u64::from(self.policy.request_timeout_ms)),
            advance(self.journal, self.store, &mut entry, fence, now),
        )
        .await;
        operation::finish(self.journal, id, entry, result, now, self.policy).await
    }
}

pub(super) async fn advance(
    journal: &DeliveryJournal,
    store: &dyn RecoveryStore,
    entry: &mut PendingRange,
    fence: &Fence,
    now: i64,
) -> Result<()> {
    match recovery::reopen(store, entry.id()).await {
        Ok(committed) => check(entry, &committed)?,
        Err(error) if error.detail == Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::NotReady)) => {
            if !entry.state.head_known {
                entry.state.expected_head = store.checkpoint_head().await?;
                entry.state.head_known = true;
                journal.save(entry).await?;
                return Ok(());
            }
            if entry.state.staged.len() > entry.manifest.batches.len() {
                return Err(DeliveryProblem::Integrity.error());
            }
            if let Some(batch) = entry.manifest.batches.get(entry.state.staged.len()) {
                let receipt = recovery::stage_batch(store, batch, &entry.prepared().batch_path(batch)).await?;
                entry.state.staged.push(receipt);
                journal.save(entry).await?;
                return Ok(());
            }
            let committed = recovery::commit_staged(
                store,
                fence,
                entry.state.expected_head.as_deref(),
                entry.manifest.clone(),
                &entry.state.staged,
            )
            .await?;
            check(entry, &committed)?;
        }
        Err(error) => return Err(error),
    }
    entry.state.staged.clear();
    entry.state.phase = DeliveryPhase::Committed;
    entry.state.retry_at_ms = now;
    entry.state.problem = None;
    entry.state.reconcile = false;
    entry.state.verification = None;
    journal.save(entry).await
}
pub(super) fn check(entry: &PendingRange, range: &recovery::CommittedRange) -> Result<()> {
    if range.manifest() != entry.manifest.as_ref() {
        return Err(DeliveryProblem::Integrity.error());
    }
    Ok(())
}
