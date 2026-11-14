use super::*;
use crate::{read::test_support::Read, rows::Rec};
use culprit_core::{ConnectionSpec, Engine, SourceState};
use mysql_async::Value;

const DIGEST: &str = "0123456789abcdef0123456789abcdef";

fn connector() -> Arc<Connector> {
    Arc::new(
        Connector::new(&ConnectionSpec {
            engine: Some(Engine::Mysql),
            host: "127.0.0.1".into(),
            port: Some(1),
            database: Some("db".into()),
            user: Some("unused".into()),
            password: Some("unused".into()),
            ..Default::default()
        })
        .unwrap(),
    )
}
fn request() -> CollectionRequest {
    CollectionRequest {
        subject: CollectionSubject::Database,
        selection: CollectionSelection::Snapshot { dataset: SnapshotDataset::RuntimeDistributions },
        budget: CollectionBudget { page_rows: 1, ..Default::default() },
    }
}
fn context(at: &str) -> Result<Vec<Rec>> {
    context_on_server(at, 1)
}

fn context_on_server(at: &str, server_id: u64) -> Result<Vec<Rec>> {
    Ok(vec![Rec::from_values(vec![
        ("database_name", "db".into()),
        ("server_id", server_id.into()),
        ("server_name", "host".into()),
        ("connection_id", 7u64.into()),
        ("version", "8.4.0".into()),
        ("login", "reader@%".into()),
        ("performance_schema", 1u64.into()),
        ("read_only", 0u64.into()),
        ("observed_at", at.into()),
    ])])
}

fn columns() -> Result<Vec<Rec>> {
    Ok(["SCHEMA_NAME", "DIGEST", "QUANTILE_95", "QUANTILE_99"]
        .into_iter()
        .map(|name| Rec::from_values(vec![("name", name.into())]))
        .collect())
}
fn histogram(low: u64, high: u64, count: u64) -> Rec {
    Rec::from_values(vec![
        ("digest", DIGEST.into()),
        ("schema_name", "db".into()),
        ("observed_at", "2020-01-01T12:00:00.300000Z".into()),
        ("low_ps", low.into()),
        ("high_ps", high.into()),
        ("runs", count.into()),
    ])
}
fn reader() -> Reader<Read> {
    let mut replies = vec![context("2020-01-01T12:00:00.100000Z"), columns()];
    replies.push(Ok(vec![Rec::from_values(vec![
        ("digest", DIGEST.into()),
        ("schema_name", "db".into()),
        ("observed_at", "2020-01-01T12:00:00.200000Z".into()),
        ("quantile_95", Value::UInt(900_000_000_000)),
        ("quantile_99", Value::UInt(u64::MAX)),
    ])]));
    replies.push(Ok(vec![histogram(0, 2_000_000_000, 970), histogram(500_000_000_000, 1_000_000_000_000, 30)]));
    replies.push(context("2020-01-01T12:00:00.400000Z"));
    replies.push(context("2020-01-01T12:00:00.500000Z"));
    replies.push(columns());
    Reader { io: Read::with(replies), connector: connector(), budget: bounded_budget(&request().budget), context: None }
}

#[tokio::test]
async fn construction_and_close_admit_no_native_work() {
    let mut source = source(connector(), &request()).unwrap();
    assert_eq!(source.query_count(), 0);
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
    assert_eq!(source.query_count(), 0);
}

#[test]
fn native_profile_preserves_every_stricter_caller_constraint() {
    let requested = CollectionBudget {
        max_queries: 9,
        max_snapshot_rows: 2,
        page_rows: 1,
        page_bytes: 16_384,
        total_bytes: 32_768,
        max_sql_bytes: 1024,
        statement_timeout_ms: 123,
        total_timeout_ms: 456,
        ..Default::default()
    };
    assert_eq!(bounded_budget(&requested), requested);
    let native = bounded_budget(&CollectionBudget::default());
    assert_eq!(native.max_queries, 24);
    assert_eq!(native.max_snapshot_rows, 256);
    assert_eq!((native.statement_timeout_ms, native.total_timeout_ms), (2000, 10_000));
}

#[tokio::test]
async fn independent_native_populations_keep_own_clocks_and_freeze_before_paging() {
    let mut source = FrozenSource::new(reader(), &request()).unwrap();
    let description = source.describe().await.unwrap();
    assert_eq!(source.query_count(), 5);
    let native = description.observation.snapshot().unwrap();
    assert_eq!(native.records, 2);
    assert_eq!(native.sources.len(), 2);
    assert!(native.sources.iter().all(|source| source.state == SourceState::Partial && !source.complete_scope));
    let CollectionRead::Page { records, next_cursor: Some(cursor) } =
        source.read_page(TelemetryStream::OperationalSamples, None).await.unwrap()
    else {
        panic!("first population")
    };
    let TelemetryRecord::Operational(OperationalObservation {
        fact: OperationalFact::RuntimeDistribution(summary),
        ..
    }) = &records[0]
    else {
        panic!("summary")
    };
    assert_eq!(summary.observed_at, "2020-01-01T12:00:00.200000Z");
    assert_eq!(summary.distribution.population.sample_count(), None);
    assert_eq!(summary.reset, ResetMarker::Unknown);
    let CollectionRead::Page { records, next_cursor: None } =
        source.read_page(TelemetryStream::OperationalSamples, Some(&cursor)).await.unwrap()
    else {
        panic!("histogram population")
    };
    let TelemetryRecord::Operational(OperationalObservation {
        fact: OperationalFact::RuntimeDistribution(histogram),
        ..
    }) = &records[0]
    else {
        panic!("histogram")
    };
    assert_eq!(histogram.observed_at, "2020-01-01T12:00:00.300000Z");
    assert_eq!(histogram.distribution.population.sample_count(), Some(1000));
    assert_eq!(histogram.distribution.p99.as_ref().unwrap().upper_ms(), Some(1000.0));
    assert_eq!(source.query_count(), 5, "warehouse paging runs no native queries");
    assert_eq!(source.finish().await.unwrap(), SourceCompletion::Verified);
    assert_eq!(source.query_count(), 7);
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
}

#[tokio::test]
async fn final_recheck_cannot_bypass_the_caller_budget_after_session_setup() {
    for max_queries in [24, 18] {
        let mut request = request();
        request.budget.max_queries = max_queries;
        let mut reader = reader();
        reader.io = reader.io.with_query_budget(bounded_budget(&request.budget).max_queries, 7).with_native_phases();
        let mut source = FrozenSource::new(reader, &request).unwrap();
        source.describe().await.unwrap();
        let mut cursor = None;
        loop {
            let CollectionRead::Page { next_cursor, .. } =
                source.read_page(TelemetryStream::OperationalSamples, cursor.as_deref()).await.unwrap()
            else {
                panic!("frozen page")
            };
            cursor = next_cursor;
            if cursor.is_none() {
                break;
            }
        }
        if max_queries == 24 {
            assert_eq!(source.finish().await.unwrap(), SourceCompletion::Verified);
        } else {
            assert_eq!(CollectionStop::from_error(&source.finish().await.unwrap_err()), CollectionStop::QueryLimit);
        }
        assert_eq!(source.query_count(), if max_queries == 24 { 19 } else { 18 });
        assert_eq!(source.close().await, SourceShutdown::Quiescent);
    }
}

#[tokio::test]
async fn uncertain_histogram_shutdown_stops_before_any_followup_metadata_query() {
    let mut reader = reader();
    reader.io.replies[3] = Err(CollectionStop::CancellationUnconfirmed.error());
    let mut source = FrozenSource::new(reader, &request()).unwrap();
    assert_eq!(
        CollectionStop::from_error(&source.describe().await.unwrap_err()),
        CollectionStop::CancellationUnconfirmed
    );
    assert_eq!(source.query_count(), 4);
    assert!(source.finish().await.is_err());
    assert_eq!(source.query_count(), 4);
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
}

#[tokio::test]
async fn histogram_permission_failure_preserves_only_the_independent_summary_and_its_own_clock() {
    let mut reader = reader();
    reader.io.replies[3] = Err(culprit_core::Error::new(ErrorCode::PermissionDenied, "native histogram denied"));
    let mut source = FrozenSource::new(reader, &request()).unwrap();
    let description = source.describe().await.unwrap();
    let native = description.observation.snapshot().unwrap();
    assert_eq!(native.records, 1);
    let histogram = native.sources.iter().find(|source| source.source == distributions::HISTOGRAM).unwrap();
    assert_eq!(histogram.state, SourceState::Unavailable);
    assert_eq!(histogram.records, 0);
    assert_eq!(histogram.error_code, Some(ErrorCode::PermissionDenied));
    let CollectionRead::Page { records, next_cursor: None } =
        source.read_page(TelemetryStream::OperationalSamples, None).await.unwrap()
    else {
        panic!("summary only")
    };
    let TelemetryRecord::Operational(OperationalObservation {
        fact: OperationalFact::RuntimeDistribution(point), ..
    }) = &records[0]
    else {
        panic!("summary")
    };
    assert_eq!(point.observed_at, "2020-01-01T12:00:00.200000Z");
    assert_eq!(point.distribution.population.sample_count(), None);
    assert_eq!(source.finish().await.unwrap(), SourceCompletion::Verified);
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
}

#[tokio::test]
async fn a_changed_native_owner_cannot_verify_the_frozen_points() {
    let mut reader = reader();
    reader.io.replies[5] = context_on_server("2020-01-01T12:00:00.500000Z", 2);
    let mut request = request();
    request.budget.page_rows = 2;
    let mut source = FrozenSource::new(reader, &request).unwrap();
    source.describe().await.unwrap();
    let CollectionRead::Page { next_cursor: None, .. } =
        source.read_page(TelemetryStream::OperationalSamples, None).await.unwrap()
    else {
        panic!("frozen populations")
    };
    assert_eq!(source.finish().await.unwrap(), SourceCompletion::Changed);
    assert_eq!(source.query_count(), 7);
    assert!(source.finish().await.is_err());
    assert_eq!(source.query_count(), 7);
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
}

#[tokio::test]
async fn a_one_row_cap_marks_the_omitted_population_and_skips_its_native_read() {
    let mut request = request();
    request.budget.max_snapshot_rows = 1;
    let mut reader = reader();
    reader.budget = bounded_budget(&request.budget);
    reader.io.replies.remove(3); // No histogram work is admitted when the summary fills the output allowance.
    let mut source = FrozenSource::new(reader, &request).unwrap();
    let description = source.describe().await.unwrap();
    let native = description.observation.snapshot().unwrap();
    assert_eq!(native.records, 1);
    assert!(native.sources.iter().all(|source| source.stopped == Some(CollectionStop::RowLimit)));
    let histogram = native.sources.iter().find(|source| source.source == distributions::HISTOGRAM).unwrap();
    assert_eq!(histogram.records, 0);
    assert_eq!(histogram.state, SourceState::Partial);
    assert_eq!(source.query_count(), 4);
    let CollectionRead::Page { records, next_cursor: None } =
        source.read_page(TelemetryStream::OperationalSamples, None).await.unwrap()
    else {
        panic!("one retained population")
    };
    assert_eq!(records.len(), 1);
    assert_eq!(source.query_count(), 4, "paging runs no native work");
    assert_eq!(source.finish().await.unwrap(), SourceCompletion::Verified);
    assert_eq!(source.query_count(), 6);
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
}
