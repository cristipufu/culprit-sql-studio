use super::*;
use crate::{
    read::{Parameter, ReadResult},
    resources::test_support::{FakeRead, instance_pressure_row, replace},
    telemetry::history_tests::{authority, native_platform, pressure},
    value::{Rec, Val},
};
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

fn initial(edition: i64, identity: Rec) -> Vec<ReadResult<Vec<Rec>>> {
    let mut rows = vec![Ok(vec![native_platform(edition)])];
    if edition != 5 {
        rows.push(Ok(vec![crate::instance::test_support::epoch(100)]));
    }
    rows.push(Ok(vec![identity]));
    rows
}
#[tokio::test]
async fn acquired_owner_is_local_and_does_not_change_the_logical_history_namespace() {
    for edition in [2, 3, 4, 5] {
        let mut rows = initial(edition, authority(edition));
        rows.push(Ok(vec![if edition == 5 { pressure() } else { instance_pressure_row() }]));
        let mut reader = FakeRead::new(rows);
        let acquisition = HistoryAcquisition::read(&mut reader, "fixture.invalid", 1433).await.unwrap();
        let owner = acquisition.owner().unwrap();
        assert!(owner.has_qualified_local_owner());
        assert_eq!(owner.database_writable, Some(true));
        assert_eq!(owner.target, acquisition.target());
        assert_eq!(owner.target.database().unwrap().incarnation.is_some(), edition == 5);
        acquisition.admit(&mut reader).await.unwrap();
        let pressure = &reader.calls.last().unwrap().sql;
        assert_eq!(pressure.contains("sys.dm_db_resource_stats"), edition == 5);
        assert_eq!(pressure.contains("sys.dm_os_schedulers"), edition != 5);
        assert_eq!(reader.calls.len(), if edition == 5 { 3 } else { 4 });
        assert_eq!(reader.close().await, SourceShutdown::Quiescent);
        assert_eq!(reader.closed, 1);
    }
}
#[tokio::test]
async fn unsupported_platforms_and_missing_native_authority_stop_before_pressure_or_history() {
    for edition in [8, 2, 3, 4, 5] {
        let identity = if edition == 8 {
            authority(edition)
        } else {
            replace(&authority(edition), &[(if edition == 5 { "replica_id" } else { "recovery_fork_guid" }, Val::Null)])
        };
        let mut reader = FakeRead::new(initial(edition, identity));
        let error = HistoryAcquisition::read(&mut reader, "fixture.invalid", 1433).await.err().unwrap();
        if edition == 8 {
            assert_eq!(error.code, ErrorCode::Unsupported);
        } else {
            assert_eq!(CollectionStop::from_error(&error), CollectionStop::SourceUnavailable);
        }
        assert_eq!(reader.calls.len(), if edition == 5 { 2 } else { 3 });
        assert!(reader.calls.iter().all(|call| !call.sql.contains("sys.database_query_store_options")));
    }
    let mut rows = initial(3, replace(&authority(3), &[("major", Val::Int(17))]));
    let Ok(metadata) = &mut rows[0] else { panic!() };
    metadata[0] = replace(&metadata[0], &[("major", Val::Text("17".into()))]);
    let mut reader = FakeRead::new(rows);
    assert_eq!(
        HistoryAcquisition::read(&mut reader, "fixture.invalid", 1433).await.err().unwrap().code,
        ErrorCode::Unsupported
    );
    assert_eq!(reader.calls.len(), 3, "a known future server is not this accepted SQL2022 compatibility profile");
}
#[tokio::test]
async fn final_native_recovery_process_or_updateability_change_cannot_complete_an_owner() {
    for changes in [
        vec![("recovery_fork_guid", Val::Text("other-native-fork".into()))],
        vec![("database_file_guid", Val::Null)],
        vec![("updateability", Val::Text("READ_ONLY".into()))],
    ] {
        let mut rows = initial(3, authority(3));
        rows.extend(initial(3, replace(&authority(3), &changes)));
        let mut reader = FakeRead::new(rows);
        let original = HistoryAcquisition::read(&mut reader, "fixture.invalid", 1433).await.unwrap();
        assert!(original.recheck(&mut reader, "fixture.invalid", 1433).await.is_err());
        assert_eq!(reader.calls.len(), 6);
        assert_eq!(reader.close().await, SourceShutdown::Quiescent);
    }
    let mut rows = initial(3, authority(3));
    let mut after = initial(3, authority(3));
    after[1] = Ok(vec![crate::instance::test_support::epoch(101)]);
    rows.extend(after);
    let mut reader = FakeRead::new(rows);
    let original = HistoryAcquisition::read(&mut reader, "fixture.invalid", 1433).await.unwrap();
    assert_eq!(
        CollectionStop::from_error(&original.recheck(&mut reader, "fixture.invalid", 1433).await.err().unwrap()),
        CollectionStop::SourceChanged
    );
}
struct FencedRead {
    inner: FakeRead,
    generation: Arc<AtomicU64>,
}
#[async_trait::async_trait]
impl NativeRead for FencedRead {
    async fn query(&mut self, sql: &str, parameters: Vec<Parameter>, limit: usize) -> ReadResult<Vec<Rec>> {
        self.inner.query(sql, parameters, limit).await
    }
    fn queries(&self) -> u32 {
        self.inner.queries()
    }
    fn connection_generation(&self) -> Option<u64> {
        Some(self.generation.load(Ordering::SeqCst))
    }
    async fn close(&mut self) -> SourceShutdown {
        self.inner.close().await
    }
}
#[tokio::test]
async fn fencing_and_native_lifecycle_stops_do_not_reenter_with_a_replacement_reader() {
    let generation = Arc::new(AtomicU64::new(1));
    let mut reader = FencedRead { inner: FakeRead::new(initial(3, authority(3))), generation: generation.clone() };
    let original = HistoryAcquisition::read(&mut reader, "fixture.invalid", 1433).await.unwrap();
    generation.store(2, Ordering::SeqCst);
    assert_eq!(
        CollectionStop::from_error(&original.admit(&mut reader).await.err().unwrap()),
        CollectionStop::SourceChanged
    );
    assert_eq!(reader.inner.calls.len(), 3, "fence is checked before another native dispatch");
    for stop in [CollectionStop::TimedOut, CollectionStop::CancellationUnconfirmed, CollectionStop::QueryLimit] {
        let mut rows = initial(3, authority(3));
        rows.push(Err(ReadError::Stopped(stop)));
        let mut reader = FakeRead::new(rows);
        reader.shutdown = SourceShutdown::Unconfirmed;
        let original = HistoryAcquisition::read(&mut reader, "fixture.invalid", 1433).await.unwrap();
        assert_eq!(CollectionStop::from_error(&original.admit(&mut reader).await.err().unwrap()), stop);
        assert_eq!(reader.calls.len(), 4);
        assert_eq!(reader.close().await, SourceShutdown::Unconfirmed);
    }
}

#[tokio::test]
async fn a_valid_final_native_clock_regression_is_source_changed_before_header_completion() {
    let mut replies = initial(3, authority(3));
    let mut final_rows =
        initial(3, replace(&authority(3), &[("observed_at", Val::Text("2020-01-01T05:59:59Z".into()))]));
    let Ok(platform) = &mut final_rows[0] else { panic!() };
    platform[0] = replace(&platform[0], &[("observed_at", Val::Text("2020-01-01T05:59:58Z".into()))]);
    replies.extend(final_rows);
    let mut reader = FakeRead::new(replies);
    let original = HistoryAcquisition::read(&mut reader, "fixture.invalid", 1433).await.unwrap();
    let final_owner = original.recheck(&mut reader, "fixture.invalid", 1433).await.unwrap();
    assert!(original.owner().unwrap().is_valid());
    assert!(final_owner.owner().unwrap().is_valid(), "each native observation is well formed within its own read");
    assert_eq!(
        CollectionStop::from_error(&original.completed(&final_owner).err().unwrap()),
        CollectionStop::SourceChanged
    );
    assert_eq!(reader.calls.len(), 6, "chronology comparison performs no repair read");
    assert_eq!(reader.close().await, SourceShutdown::Quiescent);
}
