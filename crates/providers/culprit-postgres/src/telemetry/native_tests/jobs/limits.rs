use super::support::*;
use culprit_core::{Provider, SourceState, telemetry::*};

#[tokio::test]
#[ignore = "requires nonce-owned paused Timescale jobs fixture; see dev/timescale/jobs.md"]
async fn strict_row_cap_retains_partial_job_states_and_withholds_unread_run_history() {
    let fixture = Fixture::open().await;
    let mut selected = request();
    selected.budget.max_snapshot_rows = 1;
    let captured = capture(&fixture.admin, selected).await;
    let read = &captured.snapshot;
    assert_eq!(read.observations.len(), 1);
    assert!(matches!(read.observations[0].fact, OperationalFact::TimescaleJobState(_)));
    let states = coverage(read, TIMESCALE_JOBS_SOURCE);
    assert_eq!(states.state, SourceState::Partial);
    assert_eq!(states.records, 1);
    assert_eq!(states.stopped, Some(CollectionStop::RowLimit));
    assert!(!states.complete_scope);
    let runs = coverage(read, TIMESCALE_JOB_RUNS_SOURCE);
    assert_eq!(runs.state, SourceState::Unavailable);
    assert_eq!(runs.records, 0);
    assert_eq!(runs.stopped, Some(CollectionStop::RowLimit));
    assert_eq!(
        captured.manifest.source_queries, 6,
        "no history query is dispatched after its row allowance is exhausted"
    );
}

#[tokio::test]
#[ignore = "requires nonce-owned Timescale jobs fixture; see dev/timescale/jobs.md"]
async fn native_query_and_byte_failures_close_before_returning_without_a_successful_description() {
    let fixture = Fixture::open().await;
    for stop in [CollectionStop::QueryLimit, CollectionStop::ByteLimit] {
        let mut selected = request();
        if stop == CollectionStop::QueryLimit {
            selected.budget.max_queries = 3;
        } else {
            selected.budget.page_bytes = 1024;
            selected.budget.total_bytes = 2048;
        }
        selected.validate().unwrap();
        let provider = crate::PostgresProvider::new(&fixture.admin).unwrap();
        let mut source = provider.telemetry_source(&selected).unwrap();
        assert_eq!(source.query_count(), 0);
        let result = tokio::time::timeout(CASE_TIMEOUT, source.describe()).await;
        assert_eq!(source.close().await, SourceShutdown::Quiescent);
        let error = result.expect("bounded native failure").expect_err("no successful source description");
        assert_eq!(CollectionStop::from_error(&error), stop);
        assert!(source.query_count() <= selected.budget.max_queries);
        if stop == CollectionStop::QueryLimit {
            assert_eq!(source.query_count(), 3);
        }
        let queries = source.query_count();
        assert_settled(&source.read_metrics().unwrap(), queries);
        assert!(source.describe().await.is_err());
        assert_eq!(source.close().await, SourceShutdown::Quiescent);
        assert_eq!(source.query_count(), queries);
        assert_eq!(provider.source_shutdown(), SourceShutdown::Quiescent);
    }
}

#[tokio::test]
#[ignore = "requires nonce-owned Timescale jobs fixture; see dev/timescale/jobs.md"]
async fn frozen_paging_finish_and_retirement_never_repeat_the_native_job_acquisition() {
    let fixture = Fixture::open().await;
    let provider = crate::PostgresProvider::new(&fixture.admin).unwrap();
    let mut selected = request();
    selected.budget.page_rows = 1;
    let mut source = provider.telemetry_source(&selected).unwrap();
    let description = source.describe().await.unwrap();
    let queries = source.query_count();
    assert_eq!(queries, 7);
    let mut rows = Vec::new();
    let mut cursor = None;
    loop {
        let read = source.read_page(TelemetryStream::OperationalSamples, cursor.as_deref()).await.unwrap();
        let CollectionRead::Page { records, next_cursor } = read else { panic!("bounded native job page") };
        assert!(records.len() <= 1);
        rows.extend(records.into_iter().map(|record| {
            let TelemetryRecord::Operational(row) = record else { panic!("native job point") };
            row
        }));
        assert_eq!(source.query_count(), queries);
        if next_cursor.is_none() {
            break;
        }
        assert!(rows.len() < MAX_TIMESCALE_JOB_OBSERVATIONS);
        cursor = next_cursor;
    }
    assert!(TimescaleJobsSnapshot::from_source(&description, rows).unwrap().is_valid());
    assert_eq!(source.finish().await.unwrap(), SourceCompletion::Verified);
    assert_eq!(source.query_count(), queries);
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
    assert!(source.read_page(TelemetryStream::OperationalSamples, None).await.is_err());
    assert_settled(&source.read_metrics().unwrap(), queries);
    assert_eq!(source.query_count(), queries);
    assert_eq!(provider.source_shutdown(), SourceShutdown::Quiescent);
}

#[tokio::test]
#[ignore = "requires nonce-owned Timescale jobs fixture; see dev/timescale/jobs.md"]
async fn retirement_between_frozen_pages_closes_the_owner_without_a_final_native_read() {
    let fixture = Fixture::open().await;
    let provider = crate::PostgresProvider::new(&fixture.admin).unwrap();
    let mut selected = request();
    selected.budget.page_rows = 1;
    let mut source = provider.telemetry_source(&selected).unwrap();
    source.describe().await.unwrap();
    let queries = source.query_count();
    assert!(matches!(
        source.read_page(TelemetryStream::OperationalSamples, None).await.unwrap(),
        CollectionRead::Page { next_cursor: Some(_), .. }
    ));
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
    assert!(source.finish().await.is_err());
    assert!(source.describe().await.is_err());
    assert_eq!(source.query_count(), queries);
    assert_settled(&source.read_metrics().unwrap(), queries);
    assert_eq!(provider.source_shutdown(), SourceShutdown::Quiescent);
}
