use super::*;
use std::time::Duration;

#[tokio::test]
#[ignore = "requires nonce-owned MySQL/MariaDB replication channels; run through the smoke supervisor"]
async fn actual_channel_stream_stops_at_the_callers_row_cap_without_certifying_complete_membership() {
    let fixture = Fixture::open().await;
    let mut budget = budget();
    budget.max_snapshot_rows = 1;
    let captured = capture::collect(&fixture, Login::Monitor, budget).await;
    assert_eq!(captured.manifest.status, CollectionStatus::Incomplete(CollectionStop::RowLimit));
    assert!(captured.records.len() <= 1);
    let source = captured.manifest.description.as_ref().unwrap();
    assert!(source.observation.snapshot().is_none(), "a capped roster cannot install a completed header");
    capture::show_work(
        captured.manifest.read_metrics.as_ref().unwrap(),
        fixture.native.flavor,
        &fixture.native.version,
    );
}

#[tokio::test]
#[ignore = "requires nonce-owned MySQL/MariaDB replication channels; run through the smoke supervisor"]
async fn native_channel_facts_obey_page_byte_limits_without_replacing_the_original_failure_with_empty() {
    let fixture = Fixture::open().await;
    let mut budget = budget();
    budget.page_bytes = 1024;
    let captured = capture::collect(&fixture, Login::Monitor, budget).await;
    assert_eq!(captured.manifest.status, CollectionStatus::Incomplete(CollectionStop::ByteLimit));
    assert!(captured.manifest.files.iter().all(|file| file.rows == 0 && file.status != CollectionStatus::Complete));
    capture::show_work(
        captured.manifest.read_metrics.as_ref().unwrap(),
        fixture.native.flavor,
        &fixture.native.version,
    );
}

#[tokio::test]
#[ignore = "requires nonce-owned MySQL/MariaDB replication channels; run through the smoke supervisor"]
async fn an_abandoned_real_show_page_settles_its_native_owner_without_a_second_show_or_false_completion() {
    let fixture = Fixture::open().await;
    let provider = fixture.provider(Login::Monitor);
    let mut budget = budget();
    budget.statement_timeout_ms = 750;
    let request = capture::request(budget);
    let mut source = required(provider.telemetry_source(&request), "lazy replication source");
    assert_eq!(source.query_count(), 0);
    let description = source.describe().await;
    if description.is_err() {
        assert_eq!(source.close().await, SourceShutdown::Quiescent);
    }
    required(description, "backpressure source description");
    let first = source.read_page(TelemetryStream::ReplicationSamples, None).await;
    if first.is_err() {
        assert_eq!(source.close().await, SourceShutdown::Quiescent);
    }
    let CollectionRead::Page { records, next_cursor } = required(first, "first native channel page") else {
        panic!("one native snapshot page")
    };
    assert_eq!(records.len(), 1);
    assert!(next_cursor.is_some());
    // One buffered page cannot hold the remaining row plus native completion. The existing reader deadline,
    // retention discard and cancellation owner must settle even though this consumer never asks for page two.
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let metrics = source.read_metrics().expect("native work diagnostics");
    assert!(
        metrics.is_valid() && !metrics.has_unconfirmed_work(),
        "consumer backpressure cannot retain native work indefinitely"
    );
    capture::show_work(&metrics, fixture.native.flavor, &fixture.native.version);
    assert!(
        source.finish().await.is_err(),
        "an unread channel roster cannot publish complete empty or complete subset"
    );
    let shutdown =
        tokio::time::timeout(Duration::from_secs(3), source.close()).await.expect("bounded native source close");
    assert_eq!(shutdown, SourceShutdown::Quiescent);
    let queries = source.query_count();
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
    assert_eq!(source.query_count(), queries, "retired source close must not query or retry SHOW");
    assert_eq!(provider.source_shutdown(), SourceShutdown::Quiescent);
}
