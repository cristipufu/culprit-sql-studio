//! Current observations and cold history have independent, bounded metadata admission.
use super::{
    driver::DeliveryPolicy,
    journal::{DeliveryJournal, HydrationPriority},
    recovery::{self, CommittedRange, RecoveryOrder, RecoveryStore},
    worker::fatal,
};
use culprit_core::{Result, telemetry::DeliveryProblem};

const DISCOVERY_INTERVAL_MS: i64 = 60_000;
const CURRENT_DISCOVERY_WINDOW: u32 = 32;

pub(super) struct Hydration {
    current: Lane,
    historical: Lane,
}
impl Default for Hydration {
    fn default() -> Self {
        Self {
            current: Lane::new(RecoveryOrder::NewestFirst, HydrationPriority::Current, CURRENT_DISCOVERY_WINDOW),
            historical: Lane::new(RecoveryOrder::OldestFirst, HydrationPriority::Historical, u32::MAX),
        }
    }
}
impl Hydration {
    pub async fn step(
        &mut self,
        journal: &DeliveryJournal,
        store: &dyn RecoveryStore,
        policy: DeliveryPolicy,
        now: i64,
        committed: bool,
    ) -> Result<()> {
        self.current.restart_requested |= committed;
        let current = self.current.step(journal, store, policy, now).await;
        if current.as_ref().err().is_some_and(fatal) {
            return current;
        }
        let historical = self.historical.step(journal, store, policy, now).await;
        if historical.as_ref().err().is_some_and(fatal) {
            return historical;
        }
        current.and(historical)
    }
}
struct Candidate {
    committed: CommittedRange,
    next_cursor: Option<String>,
}
struct Lane {
    order: RecoveryOrder,
    priority: HydrationPriority,
    window: u32,
    remaining: u32,
    cursor: Option<String>,
    next_scan: i64,
    next_attempt: i64,
    restart_requested: bool,
    candidate: Option<Candidate>,
}
impl Lane {
    fn new(order: RecoveryOrder, priority: HydrationPriority, window: u32) -> Self {
        Self {
            order,
            priority,
            window,
            remaining: 0,
            cursor: None,
            next_scan: 0,
            next_attempt: 0,
            restart_requested: false,
            candidate: None,
        }
    }
    async fn step(
        &mut self,
        journal: &DeliveryJournal,
        store: &dyn RecoveryStore,
        policy: DeliveryPolicy,
        now: i64,
    ) -> Result<()> {
        if now < self.next_attempt {
            return Ok(());
        }
        if self.candidate.is_some() {
            return self.admit(journal, policy, now).await;
        }
        if self.restart_requested
            || (now >= self.next_scan && (self.remaining == 0 || self.order == RecoveryOrder::NewestFirst))
        {
            self.cursor = None;
            self.remaining = self.window;
            self.next_scan = now.saturating_add(DISCOVERY_INTERVAL_MS);
            self.restart_requested = false;
        }
        if self.remaining == 0 {
            return Ok(());
        }
        let page = store.pending_ranges(self.cursor.as_deref(), 1, self.order).await?;
        if page.ranges.len() > 1 {
            return Err(DeliveryProblem::Integrity.error());
        }
        for manifest in &page.ranges {
            match journal.status(&manifest.range_id).await {
                Ok(_) => continue,
                Err(error) if error.detail == Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::NotReady)) => {}
                Err(error) => return Err(error),
            }
            let committed = recovery::reopen(store, &manifest.range_id).await?;
            if committed.manifest() != manifest {
                return Err(DeliveryProblem::Integrity.error());
            }
            // Each lane retains at most one verified manifest while its metadata budget drains.
            self.candidate = Some(Candidate { committed, next_cursor: page.next_cursor });
            return self.admit(journal, policy, now).await;
        }
        self.advance(page.next_cursor, now);
        Ok(())
    }
    async fn admit(&mut self, journal: &DeliveryJournal, policy: DeliveryPolicy, now: i64) -> Result<()> {
        let candidate = self.candidate.as_ref().ok_or_else(|| DeliveryProblem::Integrity.error())?;
        match journal.restore(&candidate.committed, self.priority).await {
            Ok(()) => (),
            Err(error) if error.detail == Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::Capacity)) => {
                self.next_attempt = now.saturating_add(i64::from(policy.retry_delay_ms));
                return Ok(());
            }
            Err(error) => return Err(error),
        }
        let candidate = self.candidate.take().ok_or_else(|| DeliveryProblem::Integrity.error())?;
        self.next_attempt = 0;
        self.advance(candidate.next_cursor, now);
        Ok(())
    }
    fn advance(&mut self, cursor: Option<String>, now: i64) {
        self.cursor = cursor;
        self.remaining = if self.cursor.is_some() { self.remaining.saturating_sub(1) } else { 0 };
        if self.remaining == 0 {
            self.cursor = None;
            self.next_scan = now.saturating_add(DISCOVERY_INTERVAL_MS);
        }
    }
}
