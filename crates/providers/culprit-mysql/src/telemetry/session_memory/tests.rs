use super::*;
use crate::{
    native::{error_log::context::tests as instance_rows, session_memory::tests as native_rows},
    read::test_support::Read,
};
use culprit_core::{Error, ErrorCode, SourceState};

fn request() -> CollectionRequest {
    CollectionRequest {
        subject: CollectionSubject::Shared { resource: instance_rows::resource() },
        selection: CollectionSelection::Snapshot { dataset: SnapshotDataset::SessionMemory },
        budget: CollectionBudget { page_rows: 1, ..Default::default() },
    }
}

fn reader() -> Reader<Read> {
    let mut replies = session_memory::tests::frame();
    replies.extend(native_rows::context_rows_at("2026-11-13T10:00:00.123459Z"));
    Reader { io: Read::with(replies), context: None }
}

async fn retained_page(source: &mut dyn TelemetrySource) -> Vec<TelemetryRecord> {
    let CollectionRead::Page { records, next_cursor: None } =
        source.read_page(TelemetryStream::OperationalSamples, None).await.unwrap()
    else {
        panic!("one complete frozen page")
    };
    assert_eq!(records.len(), 1);
    records
}

#[tokio::test]
async fn collection_uses_the_same_native_projection_as_scan_then_freezes_rows_without_requerying_pages() {
    let mut scan = Read::with(session_memory::tests::frame());
    let expected = session_memory::capture(&mut scan, 128, None).await.unwrap().snapshot.observations.remove(0);
    let mut source = FrozenSource::new(reader(), &request()).unwrap();
    let description = source.describe().await.unwrap();
    assert!(description.matches_request(&request()));
    assert_eq!(description.target, CollectionTarget::Shared(instance_rows::resource()));
    let SourceObservation::Snapshot(state) = description.observation else { panic!("snapshot") };
    assert_eq!(state.dataset, SnapshotDataset::SessionMemory);
    assert_eq!(state.sources[0].state, SourceState::Collected);
    assert_eq!(source.query_count(), 10);
    let records = retained_page(&mut source).await;
    assert_eq!(source.query_count(), 10);
    assert_eq!(records.len(), 1);
    let TelemetryRecord::Operational(row) = &records[0] else { panic!("native memory fact") };
    assert_eq!(row.key, expected.key().unwrap());
    assert_eq!(row.fact, OperationalFact::SessionMemory(Box::new(expected)));
    assert_eq!(source.finish().await.unwrap(), SourceCompletion::Verified);
    assert_eq!(source.query_count(), 14);
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
}

#[tokio::test]
async fn generation_and_consumer_changes_at_finish_cannot_verify_provisional_participants() {
    let request = request();
    let mut reader = reader();
    reader.io.replies[12] = Ok(vec![instance_rows::process(10_000, 99)]);
    let mut source = FrozenSource::new(reader, &request).unwrap();
    source.describe().await.unwrap();
    retained_page(&mut source).await;
    assert_eq!(source.finish().await.unwrap(), SourceCompletion::Changed);
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
}

#[tokio::test]
async fn the_requested_native_owner_is_established_before_any_thread_or_statement_data_read() {
    let mut request = request();
    let CollectionSubject::Shared { resource } = &mut request.subject else { panic!("shared target") };
    resource.native.id = "f54a461c-0001-4000-8000-000000000002".into();
    let mut source = FrozenSource::new(reader(), &request).unwrap();
    assert_eq!(source.describe().await.err().unwrap().detail, CollectionStop::SourceChanged.error().detail);
    assert_eq!(source.query_count(), 4);
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
}

#[tokio::test]
async fn a_known_context_permission_gap_is_published_as_partial_with_its_native_error_code() {
    let mut reader = reader();
    reader.io.replies[5] = Err(Error::new(ErrorCode::PermissionDenied, "native context source denied"));
    let mut source = FrozenSource::new(reader, &request()).unwrap();
    let description = source.describe().await.unwrap();
    let SourceObservation::Snapshot(snapshot) = description.observation else { panic!("snapshot") };
    assert_eq!(snapshot.records, 1);
    assert_eq!(snapshot.sources[0].state, SourceState::Partial);
    assert_eq!(snapshot.sources[0].error_code, Some(ErrorCode::PermissionDenied));
    assert!(!snapshot.sources[0].complete_scope);
    retained_page(&mut source).await;
    assert_eq!(source.finish().await.unwrap(), SourceCompletion::Verified);
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
}

#[tokio::test]
async fn uncertain_native_reads_cannot_run_final_checks_or_reopen_the_recipe() {
    let mut reader = reader();
    reader.io.replies[5] = Err(CollectionStop::CancellationUnconfirmed.error());
    let mut source = FrozenSource::new(reader, &request()).unwrap();
    assert!(source.describe().await.is_err());
    assert!(source.finish().await.is_err());
    assert_eq!(source.query_count(), 6);
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
}
