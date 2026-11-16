use super::*;
use crate::{
    activity::database_context::tests::row,
    instance::test_support::{epoch, metadata},
    read::{ReadError, ReadResult},
    replication::tests::{availability, geo, layout, primary, unavailable},
    resources::test_support::{FakeRead, latest_row, replace},
    value::{Rec, Val},
};
use culprit_core::{Error, ErrorCode, SourceState};
use culprit_read::ObservationReader;

const BEFORE: &str = "2020-01-01T06:01:58Z";
const AFTER: &str = "2020-01-01T06:02:01Z";
const FINAL: &str = "2020-01-01T06:02:02Z";

fn request() -> CollectionRequest {
    CollectionRequest {
        subject: CollectionSubject::Database,
        selection: CollectionSelection::Snapshot { dataset: SnapshotDataset::Replication },
        budget: CollectionBudget { page_rows: 2, ..Default::default() },
    }
}

fn authority(edition: i64, at: &str) -> Vec<ReadResult<Vec<Rec>>> {
    let platform = replace(&metadata(), &[("edition", Val::Int(edition)), ("observed_at", Val::Text(at.into()))]);
    let database = replace(&row(edition), &[("observed_at", Val::Text(at.into()))]);
    let mut replies = vec![Ok(vec![platform])];
    if edition != 5 {
        replies.push(Ok(vec![epoch(100)]));
    }
    replies.push(Ok(vec![database]));
    replies
}

fn pressure() -> Rec {
    replace(
        &latest_row("2020-01-01T06:01:59Z"),
        &[
            ("avg_cpu_percent", Val::Float(10.0)),
            ("avg_instance_cpu_percent", Val::Float(10.0)),
            ("max_worker_percent", Val::Float(0.0)),
            ("avg_data_io_percent", Val::Float(0.0)),
            ("avg_log_write_percent", Val::Float(0.0)),
        ],
    )
}

// Azure rechecks reuse their original native identity projection; boxed/MI rechecks re-observe native platform
// and instance epoch on the same held connection. These helpers declare replies, never production policy.
fn recheck(edition: i64, at: &str) -> Vec<ReadResult<Vec<Rec>>> {
    let mut replies = authority(edition, at);
    if edition == 5 {
        let _ = replies.remove(0);
    }
    replies
}

fn source(edition: i64, data: Vec<ReadResult<Vec<Rec>>>) -> ReplicationSource<FakeRead> {
    let mut replies = authority(edition, BEFORE);
    if edition == 5 {
        replies.push(Ok(vec![pressure()]));
    }
    replies.extend(data);
    replies.extend(recheck(edition, AFTER));
    replies.extend(recheck(edition, FINAL));
    ReplicationSource::with_io(FakeRead::new(replies), &request(), "fixture.invalid".into(), 1433).unwrap()
}

async fn drain(source: &mut ReplicationSource<FakeRead>) -> Vec<TelemetryRecord> {
    let mut rows = Vec::new();
    let mut cursor = None;
    loop {
        let CollectionRead::Page { records, next_cursor } =
            source.read_page(TelemetryStream::ReplicationSamples, cursor.as_deref()).await.unwrap()
        else {
            panic!("bounded replication page")
        };
        rows.extend(records);
        cursor = next_cursor;
        if cursor.is_none() {
            break;
        }
    }
    rows
}

#[tokio::test]
async fn boxed_and_managed_instance_snapshots_export_the_same_native_ag_reader_with_exact_scope_and_receipts() {
    for edition in [3, 8] {
        let hadr = if edition == 8 { Val::Null } else { Val::Int(1) };
        let mut data = vec![Ok(vec![layout(hadr, 1)]), Ok(vec![primary(), availability()])];
        if edition == 8 {
            data.push(Ok(vec![geo()]));
        }
        let mut source = source(edition, data);
        assert_eq!(source.query_count(), 0, "constructor does no native work");
        let description = source.describe().await.unwrap();
        assert!(description.partition.is_qualified());
        assert!(matches!(description.partition, SourcePartition::InstanceLocal { .. }));
        let snapshot = description.observation.snapshot().unwrap();
        assert_eq!(snapshot.dataset, SnapshotDataset::Replication);
        assert_eq!(snapshot.records, if edition == 8 { 3 } else { 2 });
        assert_eq!(snapshot.sources.len(), if edition == 8 { 2 } else { 1 });
        assert!(snapshot.sources.iter().all(|coverage| coverage.complete_scope));
        let rows = drain(&mut source).await;
        let capture = CaptureIdentity { observation_id: "controlled-replication".into(), revision: None };
        for row in &rows {
            let exported = ExportedRecord::new(&description, &capture, row.clone()).unwrap();
            assert!(exported.record_id.is_some());
        }
        let TelemetryRecord::Replication(remote) = &rows[1] else { panic!() };
        let ReplicationFact::AvailabilityGroup(native) = &remote.fact else { panic!() };
        assert_eq!(native.log_send_queue_kb.as_deref(), Some("9007199254740993"));
        assert_eq!(native.is_local, Some(false));
        assert_eq!(native.is_primary, Some(false));
        assert_eq!(source.finish().await.unwrap(), SourceCompletion::Verified);
        assert_eq!(source.query_count(), if edition == 8 { 12 } else { 11 });
        assert_eq!(source.close().await, SourceShutdown::Quiescent);
    }
}

#[tokio::test]
async fn azure_geo_uses_its_original_member_and_pressure_guard_without_ag_sql_or_primary_role_fabrication() {
    let secondary =
        replace(&geo(), &[("role", Val::Int(1)), ("lag_seconds", Val::Null), ("last_replication_at", Val::Null)]);
    let mut source = source(5, vec![Ok(vec![secondary])]);
    let description = source.describe().await.unwrap();
    assert!(description.partition.is_qualified());
    let rows = drain(&mut source).await;
    let TelemetryRecord::Replication(row) = &rows[0] else { panic!() };
    assert_eq!(row.source, crate::replication::Selection::GeoLinks.source());
    let ReplicationFact::GeoLink(link) = &row.fact else { panic!() };
    assert_eq!(link.role, Some(1));
    assert!(link.lag_seconds.is_none() && link.last_replication_at.is_none());
    let capture = CaptureIdentity { observation_id: "controlled-geo".into(), revision: None };
    let mut wrong_source = rows[0].clone();
    let TelemetryRecord::Replication(row) = &mut wrong_source else { panic!() };
    row.source = "another-native-family".into();
    assert!(ExportedRecord::new(&description, &capture, wrong_source).is_err());
    let mut wrong_dataset = description.clone();
    let SourceObservation::Snapshot(snapshot) = &mut wrong_dataset.observation else { panic!() };
    snapshot.dataset = SnapshotDataset::WaitCounters;
    assert!(ExportedRecord::new(&wrong_dataset, &capture, rows[0].clone()).is_err());
    assert_eq!(source.finish().await.unwrap(), SourceCompletion::Verified);
    assert_eq!(source.query_count(), 6);
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
}

#[tokio::test]
async fn independent_mi_permission_failures_preserve_readable_facts_and_the_original_unavailable_outcome() {
    let denied = Ok(vec![unavailable(229)]);
    let mut source = source(8, vec![Ok(vec![layout(Val::Int(1), 1)]), Ok(vec![primary(), availability()]), denied]);
    let description = source.describe().await.unwrap();
    let snapshot = description.observation.snapshot().unwrap();
    assert_eq!(snapshot.records, 2);
    assert_eq!(snapshot.sources[0].state, SourceState::Collected);
    assert_eq!(snapshot.sources[1].state, SourceState::Unavailable);
    assert_eq!(snapshot.sources[1].error_code, Some(ErrorCode::PermissionDenied));
    assert!(!snapshot.sources[1].complete_scope);
    assert_eq!(drain(&mut source).await.len(), 2);
    assert_eq!(source.finish().await.unwrap(), SourceCompletion::Verified);
}

#[tokio::test]
async fn empty_and_denied_sources_remain_distinct_even_with_zero_visible_links() {
    for failed in [false, true] {
        let reply = if failed { Ok(vec![unavailable(229)]) } else { Ok(vec![]) };
        let mut source = source(5, vec![reply]);
        let description = source.describe().await.unwrap();
        let snapshot = description.observation.snapshot().unwrap();
        assert_eq!(snapshot.records, 0);
        assert_eq!(snapshot.sources[0].state, if failed { SourceState::Unavailable } else { SourceState::Empty });
        assert_eq!(snapshot.sources[0].complete_scope, !failed);
        assert!(drain(&mut source).await.is_empty());
        assert_eq!(source.finish().await.unwrap(), SourceCompletion::Verified);
    }
}

#[tokio::test]
async fn changed_native_generations_or_roles_refuse_the_frame_without_a_fresh_family_read() {
    for field in ["database_file_guid", "recovery_fork_guid", "updateability"] {
        let mut replies = authority(3, BEFORE);
        replies.extend([Ok(vec![layout(Val::Int(1), 1)]), Ok(vec![primary(), availability()])]);
        let mut after = recheck(3, AFTER);
        let changed = replace(
            &row(3),
            &[
                ("observed_at", Val::Text(AFTER.into())),
                (
                    field,
                    Val::Text(if field == "updateability" { "READ_ONLY" } else { "another-native-generation" }.into()),
                ),
            ],
        );
        *after.last_mut().unwrap() = Ok(vec![changed]);
        replies.extend(after);
        let mut reader = Reader { io: FakeRead::new(replies), host: "fixture.invalid".into(), port: 1433, owner: None };
        let error = reader.observe(&request()).await.err().unwrap();
        assert_eq!(CollectionStop::from_error(&error), CollectionStop::SourceChanged, "{field}");
        assert_eq!(reader.io.calls.len(), 8);
        assert_eq!(
            reader
                .io
                .calls
                .iter()
                .filter(|call| call.sql.contains("FROM sys.dm_hadr_database_replica_states drs"))
                .count(),
            1
        );
        assert_eq!(reader.close().await, SourceShutdown::Quiescent);
        assert_eq!(reader.io.closed, 1);
    }
}

#[tokio::test]
async fn final_member_epoch_change_cannot_complete_the_source_and_unconfirmed_close_stays_explicit() {
    let mut replies = authority(3, BEFORE);
    replies.extend([Ok(vec![layout(Val::Int(1), 1)]), Ok(vec![primary(), availability()])]);
    replies.extend(recheck(3, AFTER));
    let mut final_context = recheck(3, FINAL);
    final_context[1] = Ok(vec![epoch(101)]);
    replies.extend(final_context);
    let mut io = FakeRead::new(replies);
    io.shutdown = SourceShutdown::Unconfirmed;
    let mut source = ReplicationSource::with_io(io, &request(), "fixture.invalid".into(), 1433).unwrap();
    source.describe().await.unwrap();
    drain(&mut source).await;
    assert_eq!(source.finish().await.unwrap(), SourceCompletion::Changed);
    assert_eq!(source.close().await, SourceShutdown::Unconfirmed);
}

#[tokio::test]
async fn caller_query_budget_stops_replication_without_post_read_native_work() {
    for allowed in [0, 3, 4] {
        let mut replies = authority(3, BEFORE);
        replies.extend([Ok(vec![layout(Val::Int(1), 1)]), Ok(vec![primary()])]);
        let mut reader = Reader {
            io: FakeRead::new(replies).with_query_limit(allowed),
            host: "fixture.invalid".into(),
            port: 1433,
            owner: None,
        };
        let error = reader.observe(&request()).await.err().unwrap();
        assert_eq!(CollectionStop::from_error(&error), CollectionStop::QueryLimit);
        assert_eq!(reader.io.calls.len() as u32, allowed);
        assert!(reader.owner.is_none());
        assert_eq!(reader.close().await, SourceShutdown::Quiescent);
    }
}

#[tokio::test]
async fn cancelled_native_reads_never_publish_a_partial_frame_or_start_a_second_family() {
    for stop in [CollectionStop::Cancelled, CollectionStop::TimedOut, CollectionStop::CancellationUnconfirmed] {
        let mut replies = authority(8, BEFORE);
        replies.extend([Ok(vec![layout(Val::Null, 1)]), Err(ReadError::Stopped(stop)), Ok(vec![geo()])]);
        let mut io = FakeRead::new(replies);
        io.shutdown = if stop == CollectionStop::CancellationUnconfirmed {
            SourceShutdown::Unconfirmed
        } else {
            SourceShutdown::Quiescent
        };
        let shutdown = io.shutdown;
        let mut reader = Reader { io, host: "fixture.invalid".into(), port: 1433, owner: None };
        let error = reader.observe(&request()).await.err().unwrap();
        assert_eq!(CollectionStop::from_error(&error), stop);
        assert_eq!(reader.io.calls.len(), 5);
        assert!(reader.owner.is_none());
        assert!(!reader.io.calls.iter().any(|call| call.sql.contains("sys.dm_geo_replication_link_status")));
        assert_eq!(reader.close().await, shutdown);
        assert_eq!(reader.io.closed, 1);
    }
}

#[tokio::test]
async fn missing_native_owner_information_retains_positive_rows_only_with_partial_qualification() {
    let mut replies = authority(3, BEFORE);
    replies[1] = Err(ReadError::Source(Error::new(ErrorCode::PermissionDenied, "controlled epoch permission")));
    replies.extend([Ok(vec![layout(Val::Int(1), 1)]), Ok(vec![primary(), availability()])]);
    let mut after = recheck(3, AFTER);
    after[1] = Err(ReadError::Source(Error::new(ErrorCode::PermissionDenied, "controlled epoch permission")));
    replies.extend(after);
    let mut reader = Reader { io: FakeRead::new(replies), host: "fixture.invalid".into(), port: 1433, owner: None };
    let captured = reader.observe(&request()).await.unwrap();
    assert_eq!(captured.records.len(), 2);
    assert!(!captured.description.partition.is_qualified());
    let snapshot = captured.description.observation.snapshot().unwrap();
    assert_eq!(snapshot.sources[0].state, SourceState::Partial);
    assert_eq!(snapshot.sources[0].error_code, Some(ErrorCode::PermissionDenied));
    assert!(!snapshot.sources[0].complete_scope);
    assert_eq!(reader.close().await, SourceShutdown::Quiescent);
}
