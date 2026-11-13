use super::*;
use crate::native::error_log::{
    context::tests::{self as context_rows, UUID},
    tests::{AT, Read, row},
};
use culprit_core::{SourceState, telemetry::CollectionRead};
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

#[tokio::test]
async fn native_resource_discovery_closes_and_never_advertises_a_database_or_an_unverified_route() {
    let request = ResourceDiscoveryRequest { kind: SharedResourceKind::Instance, budget: Default::default() };
    let mut replies = context_rows::rows();
    replies.extend(context_rows::rows());
    let mut reader = Read::with(replies);
    let discovered = discover_from(&mut reader, &request).await.unwrap();
    assert_eq!(discovered.len(), 1);
    assert_eq!(discovered[0].identity, context_rows::resource());
    assert_eq!(discovered[0].collections, [TelemetryCollection::Snapshot(SnapshotDataset::NativeFailures)]);
    assert_eq!(reader.queries.len(), 6);
    assert_eq!(reader.closed, 1);

    let mut replies = context_rows::rows();
    replies.extend(context_rows::rows());
    replies[4] = Ok(vec![context_rows::configuration(
        "f54a461c-0001-4000-8000-000000000002",
        "log_filter_internal; log_sink_internal",
    )]);
    let mut reader = Read::with(replies);
    assert_eq!(
        discover_from(&mut reader, &request).await.err().unwrap().detail,
        CollectionStop::SourceChanged.error().detail
    );
    assert_eq!(reader.closed, 1);

    let mut replies = context_rows::rows();
    replies.extend(context_rows::rows());
    let mut reader = Read::with(replies);
    reader.shutdown = Some(SourceShutdown::Unconfirmed);
    assert!(discover_from(&mut reader, &request).await.is_err());
    assert_eq!(reader.closed, 1);
    assert_eq!(UUID, discovered[0].identity.native.id);
}
