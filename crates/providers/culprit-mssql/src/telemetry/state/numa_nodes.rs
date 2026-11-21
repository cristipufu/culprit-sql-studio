//! One explicit manual shared-instance profile, with the common source owner and its one SET reservation.
use culprit_core::telemetry::*;
// Original instance2 + clocks2 + two native layout/row pairs4 + final instance2 + SET1.
pub(in crate::telemetry) const MAX_QUERIES: u32 = 11;
const MAX_ROWS: u32 = 500;
const PAGE_BYTES: u32 = 256 * 1024;
const TOTAL_BYTES: u32 = 4 * 1024 * 1024;
pub(in crate::telemetry) fn bounded_budget(caller: &CollectionBudget) -> CollectionBudget {
    CollectionBudget {
        max_queries: caller.max_queries.min(MAX_QUERIES),
        max_snapshot_rows: caller.max_snapshot_rows.min(MAX_ROWS),
        page_rows: caller.page_rows.min(MAX_ROWS),
        page_bytes: caller.page_bytes.min(PAGE_BYTES),
        total_bytes: caller.total_bytes.min(TOTAL_BYTES),
        statement_timeout_ms: caller.statement_timeout_ms.min(1000),
        total_timeout_ms: caller.total_timeout_ms.min(5000),
        ..caller.clone()
    }
}
