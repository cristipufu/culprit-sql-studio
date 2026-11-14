//! Rebuilds bounded delivery metadata from remote commits without querying a source database.
use super::{
    diagnostic_recovery::{load_diagnostic_checkpoint, load_diagnostic_input},
    journal::{DeliveryJournal, HydrationPriority},
    recovery::{self, RecoveryOrder, RecoveryPage, RecoveryStore},
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
    let page = store.pending_ranges(cursor, limit, RecoveryOrder::OldestFirst).await?;
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
        journal.restore(&committed, HydrationPriority::Historical).await?;
    }
    Ok(())
}

/// Restore the full working set before source activation; successful empty checkpoints are installed too.
pub async fn restore_diagnostics_page(
    outbox: &mut crate::outbox::Outbox,
    store: &dyn RecoveryStore,
    cursor: Option<&str>,
    limit: u32,
) -> Result<super::recovery::DiagnosticHeadPage> {
    if outbox.recovery_store_id() != Some(store.id()) {
        return Err(DeliveryProblem::DestinationChanged.error());
    }
    let page = store.diagnostic_heads(cursor, limit).await?;
    if page.heads.len() > limit as usize {
        return Err(DeliveryProblem::Capacity.error());
    }
    for head in &page.heads {
        outbox.restore_diagnostics(load_diagnostic_checkpoint(store, head).await?).await?;
    }
    Ok(page)
}
/// Published native inputs remain here until their first diagnostic checkpoint is durably committed.
/// The supplied logical label never authorizes or opens a source connection.
pub async fn restore_diagnostic_inputs_page(
    outbox: &mut crate::outbox::Outbox,
    store: &dyn RecoveryStore,
    cursor: Option<&str>,
    limit: u32,
    connection_for: impl Fn(&culprit_core::telemetry::CollectionDescription) -> Result<String>,
) -> Result<RecoveryPage> {
    use culprit_core::telemetry::SourceObservation;
    if outbox.recovery_store_id() != Some(store.id()) {
        return Err(DeliveryProblem::DestinationChanged.error());
    }
    let page = store.pending_diagnostic_inputs(cursor, limit).await?;
    if page.ranges.len() > limit as usize {
        return Err(DeliveryProblem::Capacity.error());
    }
    for manifest in &page.ranges {
        if matches!(
            manifest.source.observation,
            SourceObservation::Activity(_) | SourceObservation::FocusedPageWait { .. }
        ) {
            let connection = connection_for(&manifest.source)?;
            outbox
                .restore_diagnostic_input(&connection, load_diagnostic_input(store, &manifest.range_id).await?)
                .await?;
        }
    }
    Ok(page)
}
