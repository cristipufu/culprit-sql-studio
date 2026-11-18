use super::*;
use crate::{
    operations::native::test_support::{file_row, space_row, temp_space_row},
    resources::test_support::FakeRead,
    value::{Rec, Val},
};
use culprit_core::{SourceState, telemetry::*};
fn fresh_snapshot() -> Snapshot {
    let mut snapshot: Snapshot =
        serde_json::from_str(include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../../fixtures/mssql-demo.json")))
            .unwrap();
    snapshot.server.database = "sample".into();
    snapshot.operations = Default::default();
    snapshot.sources.clear();
    snapshot
}
fn target() -> CollectionTarget {
    culprit_core::DatabaseIdentity::server(culprit_core::Engine::Mssql, "sample", "fixture.invalid", 1433, None).into()
}
fn replies() -> Vec<crate::read::ReadResult<Vec<Rec>>> {
    vec![
        Ok(vec![space_row()]),
        Ok(vec![space_row(), file_row()]),
        Ok(vec![temp_space_row()]),
        Ok(vec![]),
        Ok(vec![Rec::of(&[("count", Val::Int(0)), ("last_native_at", Val::Null)])]),
    ]
}
#[tokio::test]
async fn scan_and_operational_stream_reuse_the_same_exact_points_only_after_confirmed_close() {
    let mut io = FakeRead::new(replies());
    let mut snapshot = fresh_snapshot();
    let mut probe = crate::probe::tests::probe(3, false);
    probe.database = "sample".into();
    collect_with(&mut io, &probe, target(), &mut snapshot).await;
    assert_eq!(io.calls.len(), 5);
    assert_eq!(io.closed, 1);
    assert_eq!(snapshot.operations.spaces.len(), 3);
    let read = snapshot.operations.storage.as_ref().unwrap();
    assert!(read.is_valid());
    assert_eq!(read.observations.len(), 4);
    assert!(read.context.native().is_none());
    assert!(
        snapshot.operations.spaces.iter().all(|space| space
            .origin
            .as_ref()
            .and_then(|reference| read.space(reference))
            .is_some())
    );
}
#[tokio::test]
async fn uncertain_shutdown_discards_staged_positives_and_preserves_explicit_unavailable_sources() {
    let mut io = FakeRead::new(replies());
    io.shutdown = SourceShutdown::Unconfirmed;
    let mut snapshot = fresh_snapshot();
    let probe = crate::probe::tests::probe(3, false);
    collect_with(&mut io, &probe, target(), &mut snapshot).await;
    assert_eq!(io.closed, 1);
    assert!(snapshot.operations.spaces.is_empty());
    let read = snapshot.operations.storage.as_ref().unwrap();
    assert!(read.observations.is_empty());
    assert!(!read.read_complete);
    assert!(read.sources.iter().all(|source| source.state == SourceState::Unavailable
        && source.stopped == Some(CollectionStop::CancellationUnconfirmed)));
}
#[tokio::test]
async fn terminal_source_failure_stops_siblings_while_denied_metadata_keeps_independent_exact_points() {
    let mut io = FakeRead::new(vec![Err(crate::read::ReadError::Stopped(CollectionStop::TimedOut))]);
    let mut snapshot = fresh_snapshot();
    let probe = crate::probe::tests::probe(3, false);
    collect_with(&mut io, &probe, target(), &mut snapshot).await;
    assert_eq!(io.calls.len(), 1);
    assert_eq!(io.closed, 1);
    assert!(
        snapshot
            .operations
            .storage
            .as_ref()
            .unwrap()
            .sources
            .iter()
            .all(|source| source.stopped == Some(CollectionStop::TimedOut))
    );
    let mut responses = replies();
    responses[1] = Err(crate::read::ReadError::Source(culprit_core::Error::new(
        culprit_core::ErrorCode::PermissionDenied,
        "denied",
    )));
    let mut io = FakeRead::new(responses);
    let mut snapshot = fresh_snapshot();
    collect_with(&mut io, &probe, target(), &mut snapshot).await;
    assert_eq!(io.calls.len(), 5);
    assert_eq!(io.closed, 1);
    let read = snapshot.operations.storage.unwrap();
    assert_eq!(read.observations.len(), 2);
    assert_eq!(
        read.sources.iter().find(|source| source.source == culprit_core::source_id::FILE_CAPACITY).unwrap().state,
        SourceState::Unavailable
    );
}
