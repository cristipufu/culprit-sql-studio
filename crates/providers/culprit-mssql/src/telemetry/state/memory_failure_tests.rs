use super::*;
use crate::{
    oom_events::tests::{platform, replace, row},
    resources::{pool::test_support::identity_row, test_support::FakeRead},
    value::{Rec, Val},
};
use culprit_core::{Engine, SourceState};

fn request() -> CollectionRequest {
    CollectionRequest {
        subject: CollectionSubject::Shared {
            resource: SharedResourceIdentity {
                engine: Engine::Mssql,
                kind: SharedResourceKind::Pool,
                namespace: NativeIdentity::new("fixture-server").qualified("kind", "azure-sql-logical-server"),
                native: NativeIdentity::new("fixture-pool"),
            },
        },
        selection: CollectionSelection::Snapshot { dataset: SnapshotDataset::MemoryFailures },
        budget: CollectionBudget { page_rows: 1, ..Default::default() },
    }
}
fn source(after: Rec, failure: Option<ReadError>) -> StateSource<FakeRead> {
    let mut replies = vec![
        Ok(vec![platform(5)]),
        Ok(vec![identity_row()]),
        Ok(vec![super::tests::clock(false, "2020-01-01T06:02:00Z")]),
        Ok(vec![super::tests::pressure(false)]),
        Ok(vec![platform(5)]),
        failure.map_or_else(|| Ok(vec![row(), row()]), Err),
    ];
    replies.extend([Ok(vec![super::tests::clock(false, "2020-01-01T06:02:01Z")]), Ok(vec![after])]);
    StateSource::with_io(FakeRead::new(replies), &request(), "member-alias.invalid".into(), 1433).unwrap()
}

#[tokio::test]
async fn explicit_pool_capture_pages_a_frozen_native_multiset_and_rechecks_the_exact_owner() {
    let mut source = source(identity_row(), None);
    let description = source.describe().await.unwrap();
    assert!(
        matches!(description.target, CollectionTarget::Shared(ref target) if target.kind==SharedResourceKind::Pool)
    );
    let snapshot = description.observation.snapshot().unwrap();
    assert_eq!(snapshot.dataset, SnapshotDataset::MemoryFailures);
    assert_eq!(snapshot.records, 2);
    assert_eq!(snapshot.sources[0].state, SourceState::Collected);
    let mut positions = vec![];
    let mut keys = vec![];
    let mut cursor = None;
    loop {
        let CollectionRead::Page { records, next_cursor } =
            source.read_page(TelemetryStream::OperationalSamples, cursor.as_deref()).await.unwrap()
        else {
            panic!()
        };
        assert_eq!(records.len(), 1);
        for record in records {
            let TelemetryRecord::Operational(observation) = record else { panic!() };
            keys.push(observation.key);
            let OperationalFact::MssqlOutOfMemory(fact) = observation.fact else { panic!() };
            positions.push(fact.position);
        }
        cursor = next_cursor;
        if cursor.is_none() {
            break;
        }
    }
    assert_eq!(positions, [0, 1]);
    assert_ne!(keys[0], keys[1]);
    assert_eq!(source.finish().await.unwrap(), SourceCompletion::Verified);
    assert_eq!(source.query_count(), 8);
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
}

#[tokio::test]
async fn a_changed_member_role_or_pool_cannot_publish_the_buffer_as_verified() {
    for after in [
        replace(identity_row(), "pool_name", Val::Text("changed-pool".into())),
        replace(identity_row(), "physical_incarnation", Val::Text("changed-process".into())),
    ] {
        let mut source = source(after, None);
        source.describe().await.unwrap();
        let _ = source.read_page(TelemetryStream::OperationalSamples, None).await.unwrap();
        let _ = source.read_page(TelemetryStream::OperationalSamples, Some("1")).await.unwrap();
        assert_eq!(source.finish().await.unwrap(), SourceCompletion::Changed);
        source.close().await;
    }
}

#[tokio::test]
async fn terminal_native_reads_stop_before_final_clock_and_identity_sql() {
    for stop in [CollectionStop::TimedOut, CollectionStop::CancellationUnconfirmed, CollectionStop::ByteLimit] {
        let mut source = source(identity_row(), Some(ReadError::Stopped(stop)));
        assert_eq!(CollectionStop::from_error(&source.describe().await.unwrap_err()), stop);
        assert_eq!(source.query_count(), 6);
        assert!(source.finish().await.is_err());
        source.close().await;
    }
}

#[test]
fn memory_failures_cannot_be_requested_as_database_owned_or_as_general_pool_operations() {
    let mut invalid = request();
    invalid.subject = CollectionSubject::Database;
    assert!(StateSource::with_io(FakeRead::new(vec![]), &invalid, "route".into(), 1433).is_err());
    let mut invalid = request();
    invalid.selection = CollectionSelection::Snapshot { dataset: SnapshotDataset::Operational };
    assert!(StateSource::with_io(FakeRead::new(vec![]), &invalid, "route".into(), 1433).is_err());
}
