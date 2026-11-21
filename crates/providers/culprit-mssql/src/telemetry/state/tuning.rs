//! The existing Tuning family owner has one finite profile on both eligible database routes.
use culprit_core::telemetry::CollectionBudget;

// Boxed: profile1 + original/final authority3+3 + clocks2 + layout1 + four native families4
// + five existing optional-read pressure admissions5. Azure retains its original15 dispatch path.
const MAX_READER_QUERIES: u32 = 19;
pub(super) const MAX_QUERIES: u32 = MAX_READER_QUERIES + crate::read::ReadSession::SESSION_SETUP_QUERIES;
pub(super) const MAX_ROWS: u32 = 128;
const PAGE_ROWS: u32 = 32;
const PAGE_BYTES: u32 = 64 * 1024;
const TOTAL_BYTES: u32 = 256 * 1024;

pub(in crate::telemetry) fn bounded_budget(requested: &CollectionBudget) -> CollectionBudget {
    CollectionBudget {
        max_queries: requested.max_queries.min(MAX_QUERIES),
        max_snapshot_rows: requested.max_snapshot_rows.min(MAX_ROWS),
        page_rows: requested.page_rows.min(PAGE_ROWS),
        page_bytes: requested.page_bytes.min(PAGE_BYTES),
        max_pages: requested.max_pages.min(MAX_ROWS),
        total_bytes: requested.total_bytes.min(TOTAL_BYTES),
        statement_timeout_ms: requested.statement_timeout_ms.min(1_000),
        total_timeout_ms: requested.total_timeout_ms.min(5_000),
        ..requested.clone()
    }
}

#[cfg(test)]
mod tests;
