//! One native archival owner, independent of warehouse requests and warehouse metadata capacity.
use super::{
    archive_driver::ArchiveDriver,
    contract::Contract,
    driver::DeliveryPolicy,
    journal::DeliveryJournal,
    recovery::{self, Fence, RecoveryStore},
    worker::{Report, fatal},
};
use crate::{capture::CollectionCancellation, outbox::OutboxDelivery};
use culprit_core::{Result, telemetry::*};
use std::sync::Arc;
use tokio::sync::Notify;
pub(super) struct ArchiveWorker<'a> {
    pub outbox: OutboxDelivery,
    pub journal: &'a mut DeliveryJournal,
    pub store: &'a dyn RecoveryStore,
    pub fence: Fence,
    pub contract: Contract,
    pub policy: DeliveryPolicy,
    pub maintenance_cursor: Option<String>,
    pub committed: Arc<Notify>,
}
impl ArchiveWorker<'_> {
    pub async fn work_once(
        &mut self,
        now: i64,
        cancellation: &CollectionCancellation,
        report: &Report<'_>,
    ) -> Result<()> {
        if cancellation.is_cancelled() {
            return Ok(());
        }
        self.journal.bind_store(self.store.id()).await?;
        let (pending, _) = self.journal.pending(0, 1).await?;
        if let Some(id) = pending.first() {
            return self.advance(id, now, cancellation, report).await;
        }
        if let Some(id) = self.outbox.next_archive().await? {
            let prepared = self.outbox.prepare(&id, self.contract.clone(), vec![]).await?;
            self.journal.register(&prepared).await?;
            return self.advance(&prepared.manifest().range_id, now, cancellation, report).await;
        }
        self.maintain(cancellation).await
    }
    async fn advance(
        &mut self,
        id: &str,
        now: i64,
        cancellation: &CollectionCancellation,
        report: &Report<'_>,
    ) -> Result<()> {
        let before = self.journal.status(id).await?;
        let result = ArchiveDriver::new(self.journal, self.store, self.policy)?.step(id, &self.fence, now).await;
        match result {
            Ok(entry) => {
                if entry.phase == DeliveryPhase::Committed && !cancellation.is_cancelled() {
                    let committed = recovery::reopen(self.store, id).await?;
                    self.outbox.mark_archived(&committed).await?;
                    self.journal.forget_archive(&committed).await?;
                    self.committed.notify_one();
                }
                report(Some(&before), Ok(entry))
            }
            Err(error) => {
                report(Some(&before), Err(error.clone()))?;
                if fatal(&error) {
                    return Err(error);
                }
                if self.journal.status(id).await?.phase == DeliveryPhase::Quarantined {
                    let entry = self.journal.load(id).await?;
                    self.outbox.quarantine_archive(&entry.manifest.capture.observation_id).await?;
                }
                Ok(())
            }
        }
    }
    async fn maintain(&mut self, cancellation: &CollectionCancellation) -> Result<()> {
        let page = self.outbox.status(32, self.maintenance_cursor.as_deref()).await?;
        for capture in page.entries {
            if cancellation.is_cancelled() {
                return Ok(());
            }
            let id = &capture.capture.observation_id;
            if capture.state != OutboxCaptureState::Ready || self.outbox.eviction_candidate(id).await? != Some(true) {
                continue;
            }
            if let Some(range) = self.outbox.archive_range(id).await? {
                self.outbox.evict(&recovery::reopen(self.store, &range).await?).await?;
            }
        }
        self.maintenance_cursor = page.next_cursor;
        Ok(())
    }
}
