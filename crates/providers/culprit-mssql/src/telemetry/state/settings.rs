//! One current Settings point reuses all four existing native catalog families with qualified local authority.
use super::super::subject::Identity;
use culprit_core::{Error, ErrorCode, Result, telemetry::CollectionBudget};

// Profile1 + original/final local authority3+3 + clocks2 + four admissions4/catalog batches4 + owned setup1.
pub(super) const MAX_QUERIES: u32 = 18;
pub(super) const MAX_ROWS: u32 = 512;
const PAGE_ROWS: u32 = 64;
const PAGE_BYTES: u32 = 64 * 1024;
const TOTAL_BYTES: u32 = 256 * 1024;
const MIN_BOXED_MAJOR: u32 = 13;
pub(in crate::telemetry) fn bounded_budget(requested: &CollectionBudget) -> CollectionBudget {
    CollectionBudget {
        max_queries: requested.max_queries.min(MAX_QUERIES),
        max_snapshot_rows: requested.max_snapshot_rows.min(MAX_ROWS),
        page_rows: requested.page_rows.min(PAGE_ROWS),
        page_bytes: requested.page_bytes.min(PAGE_BYTES),
        total_bytes: requested.total_bytes.min(TOTAL_BYTES),
        max_pages: requested.max_pages.min(MAX_ROWS),
        statement_timeout_ms: requested.statement_timeout_ms.min(1_000),
        total_timeout_ms: requested.total_timeout_ms.min(5_000),
        ..requested.clone()
    }
}
pub(super) fn require_profile(identity: &Identity) -> Result<()> {
    match identity {
        Identity::Database { .. } => Ok(()),
        Identity::LocalDatabase(owner)
            if matches!(owner.activity.edition, 2 | 3 | 4) && owner.activity.major >= MIN_BOXED_MAJOR =>
        {
            Ok(())
        }
        _ => Err(Error::new(ErrorCode::Unsupported, culprit_core::t!("collection.unsupported"))),
    }
}
#[cfg(test)]
mod tests;
