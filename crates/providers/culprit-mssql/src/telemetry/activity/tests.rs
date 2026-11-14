use super::*;

#[test]
fn baseline_budget_is_shared_by_native_and_frozen_owners_without_loosening_tighter_limits() {
    let bounded = bounded_budget(&CollectionBudget::default());
    assert_eq!(bounded.max_queries, MAX_BASELINE_QUERIES);
    assert_eq!(bounded.max_snapshot_rows, 500);
    assert_eq!(bounded.page_rows, 500.min(CollectionBudget::default().page_rows));
    assert_eq!(bounded.statement_timeout_ms, 1_000);
    assert_eq!(bounded.total_timeout_ms, 5_000);
    bounded.validate().unwrap();
    let strict = CollectionBudget {
        max_queries: 2,
        max_snapshot_rows: 3,
        page_rows: 1,
        statement_timeout_ms: 500,
        total_timeout_ms: 1_000,
        ..CollectionBudget::default()
    };
    assert_eq!(bounded_budget(&strict), strict);
}
