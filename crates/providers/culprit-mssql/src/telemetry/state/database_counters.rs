//! Explicit selected-database log counters reuse the state reader and its held local authority.
use super::super::subject::Identity;
use culprit_core::{Error, ErrorCode, Result, telemetry::*};

// Profile1 + original/final local authority3+3 + clocks2 + admission1 + counter batch1 + owned setup1.
const MAX_QUERIES: u32 = 12;
const PAGE_BYTES: u32 = 64 * 1024;
pub(in crate::telemetry) fn bounded_budget(requested: &CollectionBudget) -> CollectionBudget {
    CollectionBudget {
        max_queries: requested.max_queries.min(MAX_QUERIES),
        max_snapshot_rows: requested.max_snapshot_rows.min(1),
        page_rows: requested.page_rows.min(1),
        page_bytes: requested.page_bytes.min(PAGE_BYTES),
        total_bytes: requested.total_bytes.min(PAGE_BYTES),
        max_pages: requested.max_pages.min(1),
        statement_timeout_ms: requested.statement_timeout_ms.min(1_000),
        total_timeout_ms: requested.total_timeout_ms.min(5_000),
        ..requested.clone()
    }
}
pub(super) fn require_profile(identity: &Identity) -> Result<()> {
    match identity {
        Identity::LocalDatabase(owner) if matches!(owner.activity.edition, 2 | 3 | 4) && owner.activity.major >= 16 => {
            Ok(())
        }
        _ => Err(Error::new(ErrorCode::Unsupported, culprit_core::t!("collection.unsupported"))),
    }
}
pub(super) fn observation(snapshot: StateSnapshotDescription, identity: &Identity) -> Result<SourceObservation> {
    if snapshot.dataset != SnapshotDataset::DatabasePerformanceCounters {
        return Ok(SourceObservation::Snapshot(snapshot));
    }
    require_profile(identity)?;
    let Identity::LocalDatabase(owner) = identity else { return Err(crate::read::invalid_data()) };
    Ok(SourceObservation::DatabasePerformanceCounters {
        snapshot,
        native_database: Box::new(owner.native_identity()?),
        database_writable: owner.activity.writable.ok_or_else(crate::read::invalid_data)?,
    })
}
#[cfg(test)]
mod tests;
