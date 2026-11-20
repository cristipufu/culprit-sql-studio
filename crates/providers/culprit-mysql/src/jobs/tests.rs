use super::*;
use crate::{native::scheduled_events::test_support::*, read::test_support::Read};
fn request() -> CollectionRequest {
    CollectionRequest {
        subject: CollectionSubject::Database,
        selection: CollectionSelection::Snapshot { dataset: SnapshotDataset::Jobs },
        budget: bounded_budget(&CollectionBudget { page_rows: 1, ..Default::default() }),
    }
}
fn reader() -> Reader<Read> {
    let mut rows = replies(vec![event("first"), event("second")]);
    rows.extend(context("2026-11-20T12:00:03Z"));
    Reader {
        io: Read::with(rows).with_query_budget(MAX_QUERIES, 7).with_native_phases(),
        connector: connector(),
        context: None,
    }
}
#[tokio::test]
async fn actual_event_source_constructor_is_idle_and_frozen_pages_keep_one_original_native_owner() {
    let requested = request();
    let mut original = source(connector(), &requested).unwrap();
    assert_eq!(original.query_count(), 0);
    assert_eq!(original.close().await, SourceShutdown::Quiescent);
    let mut source = FrozenSource::new(reader(), &requested).unwrap();
    let description = source.describe().await.unwrap();
    assert!(description.matches_request(&requested));
    assert_eq!(source.query_count(), 20);
    let mut next = None;
    let mut rows = vec![];
    loop {
        let CollectionRead::Page { records, next_cursor } =
            source.read_page(TelemetryStream::OperationalSamples, next.as_deref()).await.unwrap()
        else {
            panic!()
        };
        assert!(records.len() <= 1);
        rows.extend(records);
        next = next_cursor;
        if next.is_none() {
            break;
        }
    }
    assert_eq!(rows.len(), 2);
    assert_eq!(source.query_count(), 20);
    assert_eq!(source.finish().await.unwrap(), SourceCompletion::Verified);
    assert_eq!(source.query_count(), 25);
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
}
#[tokio::test]
async fn final_original_owner_verification_honors_prepare_execute_setup_budget_and_changed_connection() {
    let mut requested = request();
    requested.budget.max_queries = 24;
    let mut reader = reader();
    reader.io = reader.io.with_query_budget(24, 7);
    let mut source = FrozenSource::new(reader, &requested).unwrap();
    source.describe().await.unwrap();
    source.read_page(TelemetryStream::OperationalSamples, None).await.unwrap();
    source.read_page(TelemetryStream::OperationalSamples, Some("1")).await.unwrap();
    assert_eq!(CollectionStop::from_error(&source.finish().await.unwrap_err()), CollectionStop::QueryLimit);
    assert_eq!(source.query_count(), 24);
    source.close().await;
    let mut reader = super::tests::reader();
    let mut rows = replies(vec![event("one")]);
    let mut changed = context("2026-11-20T12:00:03Z");
    let original = changed.remove(0).unwrap().remove(0);
    changed.insert(0, Ok(vec![original.with_fields(&[("connection_id", mysql_async::Value::UInt(56))])]));
    rows.extend(changed);
    reader.io = Read::with(rows);
    let mut source = FrozenSource::new(reader, &request()).unwrap();
    source.describe().await.unwrap();
    source.read_page(TelemetryStream::OperationalSamples, None).await.unwrap();
    assert_eq!(source.finish().await.unwrap(), SourceCompletion::Changed);
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
}
#[tokio::test]
async fn unconfirmed_close_is_never_a_successful_original_capture() {
    let mut reader = reader();
    reader.io.shutdown = Some(SourceShutdown::Unconfirmed);
    let mut source = FrozenSource::new(reader, &request()).unwrap();
    source.describe().await.unwrap();
    assert_eq!(source.close().await, SourceShutdown::Unconfirmed);
}
#[test]
fn event_profile_is_a_strict_intersection_and_never_an_automatic_cost_approval() {
    let limits = bounded_budget(&CollectionBudget::upper_limits());
    assert_eq!(limits.max_queries, 25);
    assert_eq!(limits.max_snapshot_rows, 128);
    assert_eq!(limits.statement_timeout_ms, 2000);
    assert_eq!(limits.total_timeout_ms, 10000);
    let requested = CollectionBudget {
        max_queries: 4,
        max_snapshot_rows: 2,
        page_rows: 1,
        page_bytes: 1024,
        total_bytes: 2048,
        statement_timeout_ms: 10,
        total_timeout_ms: 40,
        ..Default::default()
    };
    assert_eq!(bounded_budget(&requested), requested);
}
