//! Native archiving and analytical delivery own separate progress journals and independent I/O futures.
use super::{
    archive_worker::ArchiveWorker,
    committed_cache::CommittedRangeCache,
    contract::Contract,
    driver::{DeliveryDriver, DeliveryPolicy},
    journal::DeliveryJournal,
    recovery::{self, Fence, RecoveryStore},
    sink::TelemetrySink,
};
use crate::{capture::CollectionCancellation, lifecycle::with_background, outbox::OutboxDelivery};
use culprit_core::{Result, telemetry::*};
use std::{sync::Arc, time::Duration};
use tokio::sync::Notify;
const WORK_PAGE: u32 = 32;
const HISTORY_WORK_PAGE: u32 = WORK_PAGE / 2;
const RECENT_WORK_PAGE: u32 = WORK_PAGE - HISTORY_WORK_PAGE;
const UPDATE_POLL_MILLIS: i64 = 60_000;
const TURN_DELAY: Duration = Duration::from_millis(250);
pub(super) type Report<'a> = dyn Fn(Option<&DeliveryEntry>, Result<DeliveryEntry>) -> Result<()> + Sync + 'a;

pub struct DeliveryJournals<'a> {
    pub archive: &'a mut DeliveryJournal,
    pub analytical: &'a mut DeliveryJournal,
}
pub struct DeliveryWorker<'a> {
    archive: ArchiveWorker<'a>,
    analytical: AnalyticalWorker<'a>,
}
struct AnalyticalWorker<'a> {
    journal: &'a mut DeliveryJournal,
    store: &'a dyn RecoveryStore,
    sink: &'a dyn TelemetrySink,
    fence: Fence,
    policy: DeliveryPolicy,
    after: u64,
    next_poll: i64,
    restore_cursor: Option<String>,
    restored: bool,
    next_restore_scan: i64,
    committed: Arc<Notify>,
    capabilities: CommittedRangeCache,
}
impl<'a> DeliveryWorker<'a> {
    pub fn new(
        outbox: OutboxDelivery,
        journals: DeliveryJournals<'a>,
        recovery: &'a dyn RecoveryStore,
        sink: &'a dyn TelemetrySink,
        fence: Fence,
        contract: Contract,
        policy: DeliveryPolicy,
    ) -> Result<Self> {
        contract.validate()?;
        policy.validate()?;
        if fence.store_id() != recovery.id() || outbox.recovery_store_id() != Some(recovery.id()) {
            return Err(DeliveryProblem::Fenced.error());
        }
        let committed = Arc::new(Notify::new());
        Ok(Self {
            archive: ArchiveWorker {
                outbox,
                journal: journals.archive,
                store: recovery,
                fence: fence.clone(),
                contract,
                policy,
                maintenance_cursor: None,
                committed: committed.clone(),
            },
            analytical: AnalyticalWorker {
                journal: journals.analytical,
                store: recovery,
                sink,
                fence,
                policy,
                after: 0,
                next_poll: 0,
                restore_cursor: None,
                restored: false,
                next_restore_scan: 0,
                committed,
                capabilities: CommittedRangeCache::default(),
            },
        })
    }
    pub async fn run(&mut self, cancellation: &CollectionCancellation, report: &Report<'_>) -> Result<()> {
        let archive = async {
            while !cancellation.is_cancelled() {
                handle_turn(
                    self.archive.work_once(chrono::Utc::now().timestamp_millis(), cancellation, report).await,
                    report,
                )?;
                wait(cancellation).await;
            }
            Ok(())
        };
        let analytical = async {
            while !cancellation.is_cancelled() {
                handle_turn(
                    self.analytical.work_once(chrono::Utc::now().timestamp_millis(), cancellation, report).await,
                    report,
                )?;
                wait(cancellation).await;
            }
            Ok(())
        };
        with_background(cancellation, archive, analytical).await
    }
    #[cfg(test)]
    pub(crate) async fn work_once(
        &mut self,
        now: i64,
        cancellation: &CollectionCancellation,
        report: impl FnMut(Option<&DeliveryEntry>, Result<DeliveryEntry>) -> Result<()> + Send,
    ) -> Result<()> {
        let report = std::sync::Mutex::new(report);
        let report = |before: Option<&DeliveryEntry>, result: Result<DeliveryEntry>| {
            report.lock().map_err(|_| DeliveryProblem::Storage.error())?(before, result)
        };
        handle_turn(self.archive.work_once(now, cancellation, &report).await, &report)?;
        handle_turn(self.analytical.work_once(now, cancellation, &report).await, &report)
    }
}
impl AnalyticalWorker<'_> {
    async fn work_once(&mut self, now: i64, cancellation: &CollectionCancellation, report: &Report<'_>) -> Result<()> {
        if cancellation.is_cancelled() {
            return Ok(());
        }
        self.journal.bind_store(self.store.id()).await?;
        self.journal.compact_retired(WORK_PAGE).await?;
        self.hydrate(now).await?;
        if now >= self.next_poll {
            self.next_poll = now.saturating_add(UPDATE_POLL_MILLIS);
            DeliveryDriver::new(self.journal, self.store, self.sink, &mut self.capabilities, self.policy)?
                .poll_updates()
                .await?;
        }
        // Reserve progress for old work while admitting current observations ahead of a cold backlog.
        let (historical, next) = self.journal.pending(self.after, HISTORY_WORK_PAGE).await?;
        let mut ids = self.journal.recent_pending(RECENT_WORK_PAGE).await?;
        for id in historical {
            if !ids.contains(&id) {
                ids.push(id);
            }
        }
        for id in ids {
            if cancellation.is_cancelled() {
                return Ok(());
            }
            let before = self.journal.status(&id).await?;
            let result = DeliveryDriver::new(self.journal, self.store, self.sink, &mut self.capabilities, self.policy)?
                .step(&id, &self.fence, now)
                .await;
            let error = result.as_ref().err().filter(|error| fatal(error)).cloned();
            report(Some(&before), result)?;
            if let Some(error) = error {
                return Err(error);
            }
        }
        self.after = next.unwrap_or(0);
        Ok(())
    }
    async fn hydrate(&mut self, now: i64) -> Result<()> {
        if self.restored {
            let notified = tokio::select! {biased;_=self.committed.notified()=>true,_=std::future::ready(())=>false};
            if now < self.next_restore_scan && !notified {
                return Ok(());
            }
            self.restore_cursor = None;
            self.restored = false;
        }
        let page = self.store.pending_ranges(self.restore_cursor.as_deref(), 1).await?;
        for manifest in &page.ranges {
            match self.journal.status(&manifest.range_id).await {
                Ok(_) => continue,
                Err(error) if error.detail == Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::NotReady)) => {}
                Err(error) => return Err(error),
            }
            let committed = recovery::reopen(self.store, &manifest.range_id).await?;
            if committed.manifest() != manifest {
                return Err(DeliveryProblem::Integrity.error());
            }
            match self.journal.restore(&committed).await {
                Ok(()) => (),
                Err(error) if error.detail == Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::Capacity)) => {
                    return Ok(());
                }
                Err(error) => return Err(error),
            }
        }
        self.restore_cursor = page.next_cursor;
        self.restored = self.restore_cursor.is_none();
        if self.restored {
            self.next_restore_scan = now.saturating_add(UPDATE_POLL_MILLIS);
        }
        Ok(())
    }
}
fn handle_turn(result: Result<()>, report: &Report<'_>) -> Result<()> {
    if let Err(error) = result {
        report(None, Err(error.clone()))?;
        if fatal(&error) {
            return Err(error);
        }
    }
    Ok(())
}
async fn wait(cancellation: &CollectionCancellation) {
    tokio::select! {biased;_=cancellation.cancelled()=>{},_=tokio::time::sleep(TURN_DELAY)=>{}}
}
pub(super) fn fatal(error: &culprit_core::Error) -> bool {
    !matches!(
        error.detail,
        Some(culprit_core::ErrorDetail::Delivery(
            DeliveryProblem::Transport
                | DeliveryProblem::NotReady
                | DeliveryProblem::Capacity
                | DeliveryProblem::Conflict
                | DeliveryProblem::Integrity
        ))
    )
}
