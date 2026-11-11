//! Rebuilds bounded delivery metadata from remote commits without querying a source database.
use super::{
    activity_recovery::{ActivityRestoreBudget, load_activity_checkpoint},
    journal::DeliveryJournal,
    recovery::{self, RecoveryPage, RecoveryStore},
};
use culprit_core::{Result, telemetry::DeliveryProblem};
#[cfg(test)]
mod tests;

/// Advance the caller's durable cursor only after success. An interrupted page can be applied again safely.
pub async fn restore_page(
    journal: &DeliveryJournal,
    store: &dyn RecoveryStore,
    cursor: Option<&str>,
    limit: u32,
) -> Result<RecoveryPage> {
    journal.bind_store(store.id()).await?;
    let page = store.recover(cursor, limit).await?;
    restore_ranges(journal, store, &page, limit).await?;
    Ok(page)
}

/// Cold startup restores only ranges that have not completed verified analytical publication.
pub async fn restore_pending_page(
    journal: &DeliveryJournal,
    store: &dyn RecoveryStore,
    cursor: Option<&str>,
    limit: u32,
) -> Result<RecoveryPage> {
    journal.bind_store(store.id()).await?;
    let page = store.pending_ranges(cursor, limit).await?;
    restore_ranges(journal, store, &page, limit).await?;
    Ok(page)
}
async fn restore_ranges(
    journal: &DeliveryJournal,
    store: &dyn RecoveryStore,
    page: &RecoveryPage,
    limit: u32,
) -> Result<()> {
    if page.ranges.len() > limit as usize {
        return Err(DeliveryProblem::Capacity.error());
    }
    for manifest in &page.ranges {
        let committed = recovery::reopen(store, &manifest.range_id).await?;
        if committed.manifest() != manifest {
            return Err(DeliveryProblem::Integrity.error());
        }
        journal.restore(&committed).await?;
    }
    Ok(())
}

/// Recover bounded incident state before activating a source owner. Empty heads are installed too.
pub async fn restore_activity_page(
    outbox: &mut crate::outbox::Outbox,
    store: &dyn RecoveryStore,
    cursor: Option<&str>,
    limit: u32,
    budget: ActivityRestoreBudget,
) -> Result<super::recovery::ActivityHeadPage> {
    if outbox.recovery_store_id() != Some(store.id()) {
        return Err(DeliveryProblem::DestinationChanged.error());
    }
    let page = store.activity_heads(cursor, limit).await?;
    if page.heads.len() > limit as usize {
        return Err(DeliveryProblem::Capacity.error());
    }
    for head in &page.heads {
        outbox.restore_activity(load_activity_checkpoint(store, head, budget).await?).await?;
    }
    Ok(page)
}
