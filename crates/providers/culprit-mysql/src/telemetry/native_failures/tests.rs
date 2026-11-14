use super::*;
use crate::native::error_log::{
    context::tests as context_rows,
    tests::{AT, row},
};
use crate::read::test_support::Read;
use culprit_core::{Engine, SourceState, telemetry::CollectionRead};
use mysql_async::Value;

fn request() -> CollectionRequest {
    CollectionRequest {
        subject: CollectionSubject::Shared { resource: context_rows::resource() },
        selection: CollectionSelection::Snapshot { dataset: SnapshotDataset::NativeFailures },
        budget: CollectionBudget { page_rows: 1, ..Default::default() },
    }
}

fn rows(count: usize) -> Vec<crate::rows::Rec> {
    (0..count).map(|_| row(AT, Value::UInt(u64::MAX), Value::from("MY-999999"))).collect()
}

fn reader(count: usize) -> Reader<Read> {
    let mut replies = native_failures::tests::frame(rows(count));
    replies.extend(context_rows::rows());
    Reader { io: Read::with(replies), context: None }
}

#[tokio::test]
async fn constructing_a_native_failure_capture_does_not_connect_or_run_setup() {
    let connector = Arc::new(
        Connector::new(&culprit_core::ConnectionSpec {
            engine: Some(culprit_core::Engine::Mysql),
            host: "127.0.0.1".into(),
            port: Some(1),
            user: Some("unused".into()),
            password: Some("unused".into()),
            ..Default::default()
        })
        .unwrap(),
    );
    let mut source = super::source(connector, &request()).unwrap();
    assert_eq!(source.query_count(), 0);
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
    assert_eq!(source.query_count(), 0);
}

#[tokio::test]
async fn final_verification_cannot_bypass_the_caller_budget_after_owned_setup() {
    const SETUP_QUERIES: u32 = 7;
    for limit in [24, 17] {
        let mut requested = request();
        requested.budget.max_queries = limit;
        let limits = native_failures::bounded_budget(&requested.budget);
        let mut reader = reader(1);
        reader.io = reader.io.with_query_budget(limits.max_queries, SETUP_QUERIES).with_native_phases();
        let mut source = FrozenSource::new(reader, &requested).unwrap();
        source.describe().await.unwrap();
        assert_eq!(source.query_count(), 15);
        let _ = source.read_page(TelemetryStream::OperationalSamples, None).await.unwrap();
        if limit == 24 {
            assert_eq!(source.finish().await.unwrap(), SourceCompletion::Verified);
            assert_eq!(source.query_count(), 18);
        } else {
            assert_eq!(CollectionStop::from_error(&source.finish().await.unwrap_err()), CollectionStop::QueryLimit);
            assert_eq!(source.query_count(), 17);
        }
        assert_eq!(source.close().await, SourceShutdown::Quiescent);
    }
}

#[tokio::test]
async fn frozen_native_rows_share_scan_decoder_and_never_requery_or_attach_current_threads_while_paging() {
    let request = request();
    let mut source = FrozenSource::new(reader(2), &request).unwrap();
    let description = source.describe().await.unwrap();
    assert_eq!(description.target, CollectionTarget::Shared(context_rows::resource()));
    assert!(description.matches_request(&request));
    let SourceObservation::Snapshot(snapshot) = &description.observation else { panic!("native snapshot") };
    assert_eq!(snapshot.dataset, SnapshotDataset::NativeFailures);
    assert_eq!(snapshot.records, 2);
    assert_eq!(snapshot.sources[0].state, SourceState::Collected);
    assert_eq!(source.query_count(), 7);
    let CollectionRead::Page { records: first, next_cursor } =
        source.read_page(TelemetryStream::OperationalSamples, None).await.unwrap()
    else {
        panic!("first frozen page")
    };
    assert_eq!(first.len(), 1);
    let CollectionRead::Page { records: second, next_cursor: None } =
        source.read_page(TelemetryStream::OperationalSamples, next_cursor.as_deref()).await.unwrap()
    else {
        panic!("second frozen page")
    };
    assert_eq!(source.query_count(), 7, "pages cannot repeat a native read");
    let expected = error_log::decode(&row(AT, Value::UInt(u64::MAX), Value::from("MY-999999")), 0).unwrap();
    let TelemetryRecord::Operational(observation) = &first[0] else { panic!("native fact") };
    assert_eq!(observation.key, expected.key().unwrap());
    assert_eq!(observation.source, sources::NATIVE_FAILURES.name);
    assert_eq!(observation.fact, OperationalFact::MysqlErrorLog(Box::new(expected)));
    let TelemetryRecord::Operational(second) = &second[0] else { panic!("second native fact") };
    assert_ne!(observation.key, second.key, "duplicate metadata retain separate capture positions");
    assert_eq!(source.finish().await.unwrap(), SourceCompletion::Verified);
    assert_eq!(source.query_count(), 10);
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
}

#[tokio::test]
async fn a_known_limit_is_partial_and_changed_native_context_cannot_complete_the_capture() {
    let mut request = request();
    request.budget.max_snapshot_rows = 2;
    let mut source = FrozenSource::new(reader(3), &request).unwrap();
    let description = source.describe().await.unwrap();
    let SourceObservation::Snapshot(snapshot) = description.observation else { panic!("snapshot") };
    assert_eq!(snapshot.records, 2);
    assert_eq!(snapshot.sources[0].state, SourceState::Partial);
    assert!(!snapshot.sources[0].complete_scope);
    source.close().await;

    let mut reader = reader(1);
    reader.io.replies.pop_back();
    reader.io.replies.push_back(Ok(vec![context_rows::process(10_000, 99)]));
    let mut source = FrozenSource::new(reader, &request).unwrap();
    source.describe().await.unwrap();
    source.read_page(TelemetryStream::OperationalSamples, None).await.unwrap();
    assert_eq!(source.finish().await.unwrap(), SourceCompletion::Changed);
    assert_eq!(source.query_count(), 10);
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
}

#[tokio::test]
async fn unknown_subjects_and_mismatched_native_owners_fail_before_the_error_log_query() {
    let mut requested = request();
    for subject in [
        CollectionSubject::Database,
        CollectionSubject::Shared {
            resource: SharedResourceIdentity { kind: SharedResourceKind::Pool, ..context_rows::resource() },
        },
        CollectionSubject::Shared {
            resource: SharedResourceIdentity { engine: Engine::Postgres, ..context_rows::resource() },
        },
    ] {
        requested.subject = subject;
        let mut reader = Reader { io: Read::default(), context: None };
        assert!(reader.observe(&requested).await.is_err());
        assert_eq!(reader.query_count(), 0);
    }
    let mut requested = request();
    let CollectionSubject::Shared { resource } = &mut requested.subject else { panic!() };
    resource.native.id = "f54a461c-0001-4000-8000-000000000002".into();
    let mut reader = Reader { io: Read::with(context_rows::rows()), context: None };
    assert_eq!(reader.observe(&requested).await.err().unwrap().detail, CollectionStop::SourceChanged.error().detail);
    assert_eq!(reader.query_count(), 3);
}

#[tokio::test]
async fn a_terminal_native_failure_cannot_run_final_metadata_rechecks_or_a_source_fallback() {
    let mut replies = context_rows::rows();
    replies.push(Err(CollectionStop::CancellationUnconfirmed.error()));
    let mut source = FrozenSource::new(Reader { io: Read::with(replies), context: None }, &request()).unwrap();
    assert!(source.describe().await.is_err());
    assert!(source.finish().await.is_err());
    assert_eq!(source.query_count(), 4);
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
}
