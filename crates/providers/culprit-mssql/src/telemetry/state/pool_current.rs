//! One closed Pool WaitCounters profile, sharing StateSource, membership, clocks and one SET reservation.
use super::*;
const MAX_TOTAL_QUERIES: u32 = 8;
const MAX_CURRENT_ROWS: u32 = 500;
const MAX_PAGE_BYTES: u32 = 256 * 1024;
const MAX_TOTAL_BYTES: u32 = 1024 * 1024;
const STATEMENT_TIMEOUT_MS: u32 = 1000;
const TOTAL_TIMEOUT_MS: u32 = 5000;
pub(super) fn selected(request: &CollectionRequest) -> bool {
    request.selection == (CollectionSelection::Snapshot { dataset: SnapshotDataset::WaitCounters })
        && matches!(&request.subject,CollectionSubject::Shared{resource} if resource.engine==culprit_core::Engine::Mssql && resource.kind==SharedResourceKind::Pool)
}
pub(in crate::telemetry) fn bounded_budget(caller: &CollectionBudget) -> CollectionBudget {
    CollectionBudget {
        max_queries: caller.max_queries.min(MAX_TOTAL_QUERIES),
        max_snapshot_rows: caller.max_snapshot_rows.min(MAX_CURRENT_ROWS),
        page_rows: caller.page_rows.min(MAX_CURRENT_ROWS),
        page_bytes: caller.page_bytes.min(MAX_PAGE_BYTES),
        total_bytes: caller.total_bytes.min(MAX_TOTAL_BYTES),
        statement_timeout_ms: caller.statement_timeout_ms.min(STATEMENT_TIMEOUT_MS),
        total_timeout_ms: caller.total_timeout_ms.min(TOTAL_TIMEOUT_MS),
        ..caller.clone()
    }
}
pub(super) async fn subject<R: NativeRead>(
    reader: &mut R,
    requested: &CollectionSubject,
) -> Result<subject::SubjectRead> {
    super::super::description::ensure_supported(reader).await?;
    let identity =
        subject::Identity::PoolCurrent(Box::new(crate::resources::pool::CurrentContext::read(reader).await?));
    if !requested.accepts(&identity.target()) {
        return Err(CollectionStop::SourceChanged.error());
    }
    Ok(subject::SubjectRead { identity, configured_log_rates: Ok(None) })
}
pub(super) async fn closing<R: NativeRead>(
    owner: &subject::Identity,
    reader: &mut R,
    from: &str,
) -> Result<Option<(String, bool, MssqlPoolCurrentAuthority)>> {
    let subject::Identity::PoolCurrent(before) = owner else { return Ok(None) };
    let after = before.recheck(reader).await?;
    if !before.confirms(&after, from) {
        return Err(CollectionStop::SourceChanged.error());
    }
    Ok(Some((
        after.point.observed_at.clone(),
        after.metadata_visible,
        MssqlPoolCurrentAuthority { before: before.point.clone(), after: after.point },
    )))
}

#[cfg(test)]
mod tests;
