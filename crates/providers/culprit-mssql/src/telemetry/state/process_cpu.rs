//! Explicit boxed CPU details share StateSource; every existing setup and final dispatch is paid.
use culprit_core::telemetry::*;
// SET1 + instance2 + clock1 + topology1 + pool1 + retained history1 + clock1 + final instance2.
pub(in crate::telemetry) fn bounded_budget(caller: &CollectionBudget) -> CollectionBudget {
    CollectionBudget {
        max_queries: caller.max_queries.min(10),
        max_snapshot_rows: caller.max_snapshot_rows.min(MAX_MSSQL_CPU_OBSERVATIONS),
        page_rows: caller.page_rows.min(MAX_MSSQL_CPU_OBSERVATIONS),
        page_bytes: caller.page_bytes.min(64 * 1024),
        total_bytes: caller.total_bytes.min(256 * 1024),
        statement_timeout_ms: caller.statement_timeout_ms.min(1000),
        total_timeout_ms: caller.total_timeout_ms.min(5000),
        ..caller.clone()
    }
}
#[cfg(test)]
mod tests;
