use super::support::*;
use culprit_core::telemetry::CollectionStop;
use futures_util::FutureExt;

#[tokio::test]
#[ignore = "requires the explicit nonce-owned PostgreSQL primary/standby topology"]
async fn strict_query_and_row_allowances_withhold_completion_and_retire_the_real_replication_reader() {
    run_case(|fixture| {
        async move {
            let specification = fixture.primary_spec.clone();
            let mut queries = request();
            queries.budget.max_queries = 3;
            fixture.stopped_capture(&specification, queries, CollectionStop::QueryLimit, false).await;
            let mut rows = request();
            rows.budget.max_snapshot_rows = 1;
            fixture.stopped_capture(&specification, rows, CollectionStop::RowLimit, false).await;
        }
        .boxed_local()
    })
    .await;
}

#[tokio::test]
#[ignore = "requires the explicit nonce-owned PostgreSQL primary/standby topology"]
async fn cancellation_after_a_real_native_page_closes_once_without_admitting_a_final_recheck() {
    run_case(|fixture| {
        async move {
            let specification = fixture.primary_spec.clone();
            fixture.stopped_capture(&specification, request(), CollectionStop::Cancelled, true).await;
        }
        .boxed_local()
    })
    .await;
}
