//! Shared policy for manual scans and Live observations, independent of native drivers.
use culprit_core::{
    limits::{MAX_RESULT_BYTES, MAX_RESULT_ROWS, OBSERVE_TIMEOUT},
    telemetry::CollectionBudget,
};
use std::time::Duration;

const QUERY_ALLOWANCE: u32 = 512;
const OVERHEAD: Duration = Duration::from_secs(60);
const MAX_DURATION: Duration = Duration::from_secs(15 * 60);

pub fn application_budget(window: Duration) -> CollectionBudget {
    CollectionBudget {
        max_snapshot_rows: MAX_RESULT_ROWS as u32,
        max_queries: QUERY_ALLOWANCE,
        page_bytes: MAX_RESULT_BYTES as u32,
        statement_timeout_ms: OBSERVE_TIMEOUT.as_millis() as u32,
        total_timeout_ms: window.saturating_add(OVERHEAD).min(MAX_DURATION).as_millis() as u32,
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn observation_windows_include_bounded_overhead_without_extending_statement_timeouts() {
        for window in [Duration::ZERO, Duration::from_secs(90), Duration::MAX] {
            let budget = application_budget(window);
            budget.validate().unwrap();
            assert_eq!(budget.statement_timeout_ms, OBSERVE_TIMEOUT.as_millis() as u32);
        }
        assert_eq!(application_budget(Duration::ZERO).total_timeout_ms, 60_000);
        assert_eq!(application_budget(Duration::from_secs(90)).total_timeout_ms, 150_000);
        assert_eq!(application_budget(Duration::MAX).total_timeout_ms, 900_000);
    }
}
