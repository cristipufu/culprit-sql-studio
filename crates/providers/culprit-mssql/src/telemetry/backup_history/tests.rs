use super::*;
use crate::{
    backup_history::tests::{self as native, Reader, Trace},
    instance::test_support::epoch,
    read::ReadResult,
    resources::test_support::{instance_pressure_row, latest_row, replace},
    value::{Rec, Val},
};
use culprit_core::{Error, ErrorCode, SourceState};
use std::sync::atomic::Ordering;
fn request() -> CollectionRequest {
    CollectionRequest {
        subject: CollectionSubject::Database,
        selection: CollectionSelection::Snapshot { dataset: SnapshotDataset::BackupHistory },
        budget: CollectionBudget { page_rows: 1, ..Default::default() },
    }
}
fn admission(edition: i64) -> Rec {
    if edition != 5 {
        return replace(&instance_pressure_row(), &[("observed_at", Val::Text("2020-01-01T06:00:00Z".into()))]);
    }
    replace(
        &latest_row("2020-01-01T05:59:59Z"),
        &[
            ("observed_at", Val::Text("2020-01-01T06:00:00Z".into())),
            ("avg_cpu_percent", Val::Float(0.0)),
            ("avg_instance_cpu_percent", Val::Float(0.0)),
            ("max_worker_percent", Val::Float(0.0)),
            ("avg_data_io_percent", Val::Float(0.0)),
            ("avg_log_write_percent", Val::Float(0.0)),
        ],
    )
}
fn replies(edition: i64, records: Vec<Rec>) -> Vec<ReadResult<Vec<Rec>>> {
    let mut rows = native::authority_replies(edition, "2020-01-01T06:00:00Z");
    rows.extend([
        Ok(vec![native::metadata_row(edition, "2020-01-01T06:00:00Z")]),
        Ok(vec![admission(edition)]),
        Ok(records),
        Ok(vec![native::metadata_row(edition, "2020-01-01T06:00:01Z")]),
    ]);
    if edition == 5 {
        rows.extend(native::authority_replies(edition, "2020-01-01T06:00:02Z"));
    } else {
        rows.extend([
            Ok(vec![native::platform(edition, "2020-01-01T06:00:02Z")]),
            Ok(vec![epoch(100)]),
            Ok(vec![native::local(edition, "2020-01-01T06:00:02Z")]),
        ]);
    }
    rows.push(Ok(vec![native::metadata_row(edition, "2020-01-01T06:00:02Z")]));
    rows
}
fn source(rows: Vec<ReadResult<Vec<Rec>>>, request: &CollectionRequest) -> (BackupHistorySource<Reader>, Arc<Trace>) {
    let mut reader = native::reader(rows);
    reader.inner = reader.inner.with_query_limit(request.budget.max_queries);
    let trace = reader.trace.clone();
    (BackupHistorySource::with_io(reader, request, "fixture.invalid".into(), 1433).unwrap(), trace)
}
async fn drain(source: &mut BackupHistorySource<Reader>) -> Vec<TelemetryRecord> {
    let mut cursor = None;
    let mut rows = Vec::new();
    loop {
        let CollectionRead::Page { records, next_cursor } =
            source.read_page(TelemetryStream::OperationalSamples, cursor.as_deref()).await.unwrap()
        else {
            panic!("bounded native backup points")
        };
        rows.extend(records);
        cursor = next_cursor;
        if cursor.is_none() {
            return rows;
        }
    }
}
#[tokio::test]
async fn azure_boxed_and_mi_sources_use_the_original_authority_exact_paid_profiles_and_single_close() {
    for edition in [3, 5, 8] {
        let (mut source, trace) = source(
            replies(edition, vec![if edition == 5 { native::azure_row() } else { native::msdb_row() }]),
            &request(),
        );
        let description = source.describe().await.unwrap();
        let context = backup_history_context(&description).unwrap();
        assert_eq!(
            description.target.database().unwrap().incarnation.as_deref(),
            if edition == 5 { Some(native::DATABASE) } else { None }
        );
        assert_eq!(context.has_qualified_local_owner(), edition != 5);
        assert_eq!(description.observation.snapshot().unwrap().sources[0].state, SourceState::Partial);
        let rows = drain(&mut source).await;
        assert_eq!(rows.len(), 1);
        assert!(
            ExportedRecord::new(
                &description,
                &CaptureIdentity { observation_id: "original-backups".into(), revision: None },
                rows[0].clone()
            )
            .is_ok()
        );
        assert_eq!(source.finish().await.unwrap(), SourceCompletion::Verified);
        assert_eq!(source.query_count(), if edition == 5 { 9 } else { 12 });
        assert_eq!(source.close().await, SourceShutdown::Quiescent);
        assert_eq!(trace.closed.load(Ordering::SeqCst), 1);
        let sql = trace.sql.lock().unwrap();
        assert_eq!(
            sql.iter()
                .filter(|sql| sql.contains(if edition == 5 {
                    "FROM sys.dm_database_backups b"
                } else {
                    "FROM msdb.dbo.backupset b"
                }))
                .count(),
            1
        );
        if edition != 5 {
            assert!(sql.iter().all(|sql| !sql.contains("sys.dm_user_db_resource_governance")));
        }
    }
}
#[tokio::test]
async fn empty_is_qualified_retained_history_and_permission_denial_is_unavailable_not_zero() {
    for denied in [false, true] {
        let mut rows = replies(3, vec![]);
        if denied {
            rows[6] = Err(ReadError::Source(Error::new(ErrorCode::PermissionDenied, "denied")));
        }
        let (mut source, trace) = source(rows, &request());
        let description = source.describe().await.unwrap();
        let native = description.observation.snapshot().unwrap();
        assert_eq!(native.records, 0);
        assert!(!native.sources[0].complete_scope);
        assert_eq!(native.sources[0].state, if denied { SourceState::Unavailable } else { SourceState::Partial });
        assert_eq!(native.sources[0].error_code, denied.then_some(ErrorCode::PermissionDenied));
        assert!(drain(&mut source).await.is_empty());
        assert_eq!(source.finish().await.unwrap(), SourceCompletion::Verified);
        assert_eq!(source.query_count(), 12);
        assert_eq!(source.close().await, SourceShutdown::Quiescent);
        assert_eq!(trace.closed.load(Ordering::SeqCst), 1);
    }
}
#[tokio::test]
async fn hyperscale_and_unknown_deployment_never_query_a_false_empty_population() {
    for tier in [Some("Hyperscale"), Some("future-tier"), None] {
        let mut rows = replies(5, vec![]);
        rows.remove(4).expect("the omitted boxed authority reply must be valid");
        rows.remove(3).expect("the omitted boxed authority reply must be valid");
        for index in [2, 3, 6] {
            let row = native::metadata_row(
                5,
                if index == 2 {
                    "2020-01-01T06:00:00Z"
                } else if index == 3 {
                    "2020-01-01T06:00:01Z"
                } else {
                    "2020-01-01T06:00:02Z"
                },
            );
            rows[index] = Ok(vec![replace(
                &row,
                &[("database_edition", tier.map(|tier| Val::Text(tier.into())).unwrap_or(Val::Null))],
            )]);
        }
        let (mut source, trace) = source(rows, &request());
        let description = source.describe().await.unwrap();
        let SourceObservation::BackupHistory { scope, snapshot, .. } = &description.observation else { unreachable!() };
        assert_eq!(scope.hyperscale, if tier == Some("Hyperscale") { Some(true) } else { None });
        assert_eq!(snapshot.sources[0].state, SourceState::Unavailable);
        assert!(drain(&mut source).await.is_empty());
        assert_eq!(source.finish().await.unwrap(), SourceCompletion::Verified);
        assert_eq!(source.query_count(), 7);
        assert_eq!(source.close().await, SourceShutdown::Quiescent);
        assert!(trace.sql.lock().unwrap().iter().all(|sql| !sql.contains("FROM sys.dm_database_backups b")));
    }
}
#[tokio::test]
async fn caller_row_limit_keeps_one_bounded_prefix_after_validating_the_native_lookahead() {
    let second = replace(
        &native::msdb_row(),
        &[
            ("backup_set_id", Val::Int(2)),
            ("backup_set_uuid", Val::Text("60000000-0000-0000-0000-000000000006".into())),
        ],
    );
    let mut limited = request();
    limited.budget.max_snapshot_rows = 1;
    let (mut source, _) = source(replies(3, vec![native::msdb_row(), second.clone()]), &limited);
    let description = source.describe().await.unwrap();
    let coverage = &description.observation.snapshot().unwrap().sources[0];
    assert_eq!(coverage.records, 1);
    assert_eq!(coverage.stopped, Some(CollectionStop::RowLimit));
    assert_eq!(drain(&mut source).await.len(), 1);
    assert_eq!(source.finish().await.unwrap(), SourceCompletion::Verified);
    source.close().await;
    let foreign = replace(&second, &[("database_guid", Val::Text(native::PHYSICAL.into()))]);
    let (mut source, _) = self::source(replies(3, vec![native::msdb_row(), foreign]), &limited);
    assert!(source.describe().await.is_err());
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
}
#[tokio::test]
async fn frame_role_or_database_drift_cannot_be_repaired_by_the_same_final_authority() {
    for (field, value) in [
        ("selected_database_id", Val::Int(8)),
        ("selected_database_guid", Val::Text(native::PHYSICAL.into())),
        ("selected_recovery_fork_guid", Val::Text(native::PHYSICAL.into())),
        ("updateability", Val::Text("READ_ONLY".into())),
        ("updateability", Val::Null),
    ] {
        let mut rows = replies(3, vec![]);
        rows[6] = Ok(vec![replace(&native::msdb_row(), &[(field, value)])]);
        let (mut source, trace) = source(rows, &request());
        assert!(source.describe().await.is_err(), "{field}");
        assert_eq!(source.query_count(), 7);
        assert_eq!(trace.sql.lock().unwrap().len(), 7);
        assert_eq!(source.close().await, SourceShutdown::Quiescent);
    }
}
#[tokio::test]
async fn final_recreation_process_role_socket_or_metadata_changes_withhold_verified_completion() {
    for change in ["guid", "fork", "file", "role", "process", "metadata", "clock", "socket"] {
        let mut rows = replies(3, vec![native::msdb_row()]);
        match change {
            "process" => rows[9] = Ok(vec![epoch(101)]),
            "metadata" => {
                rows[11] = Ok(vec![replace(
                    &native::metadata_row(3, "2020-01-01T06:00:02Z"),
                    &[("layout_available", Val::Int(0))],
                )])
            }
            "clock" => rows[10] = Ok(vec![native::local(3, "2019-01-01T06:00:02Z")]),
            "socket" => {}
            _ => {
                let field = match change {
                    "guid" => "database_guid",
                    "fork" => "recovery_fork_guid",
                    "file" => "database_file_guid",
                    _ => "updateability",
                };
                rows[10] = Ok(vec![replace(
                    &native::local(3, "2020-01-01T06:00:02Z"),
                    &[(field, Val::Text(if change == "role" { "READ_ONLY" } else { native::PHYSICAL }.into()))],
                )]);
            }
        }
        let mut reader = native::reader(rows);
        if change == "socket" {
            reader.change_at = Some(9);
        }
        let mut source = BackupHistorySource::with_io(reader, &request(), "fixture.invalid".into(), 1433).unwrap();
        source.describe().await.unwrap();
        drain(&mut source).await;
        assert!(!matches!(source.finish().await, Ok(SourceCompletion::Verified)), "{change}");
        assert_eq!(source.close().await, SourceShutdown::Quiescent);
    }
}
#[tokio::test]
async fn cancellation_fences_limits_and_missing_native_owner_never_dispatch_followup_history_work() {
    for stopped in [
        CollectionStop::Cancelled,
        CollectionStop::CancellationUnconfirmed,
        CollectionStop::TimedOut,
        CollectionStop::Pressure,
        CollectionStop::ByteLimit,
    ] {
        let mut rows = replies(3, vec![]);
        rows[5] = Err(ReadError::Stopped(stopped));
        let mut reader = native::reader(rows);
        reader.inner.shutdown = if stopped == CollectionStop::CancellationUnconfirmed {
            SourceShutdown::Unconfirmed
        } else {
            SourceShutdown::Quiescent
        };
        let shutdown = reader.inner.shutdown;
        let trace = reader.trace.clone();
        let mut source = BackupHistorySource::with_io(reader, &request(), "fixture.invalid".into(), 1433).unwrap();
        assert_eq!(CollectionStop::from_error(&source.describe().await.unwrap_err()), stopped);
        assert_eq!(source.query_count(), 6);
        assert_eq!(trace.sql.lock().unwrap().len(), 6);
        assert_eq!(source.close().await, shutdown);
    }
    for generation in [None, Some(0)] {
        let mut reader = native::reader(replies(3, vec![]));
        reader.generation = generation;
        let mut source = BackupHistorySource::with_io(reader, &request(), "fixture.invalid".into(), 1433).unwrap();
        assert_eq!(
            CollectionStop::from_error(&source.describe().await.unwrap_err()),
            CollectionStop::SourceUnavailable
        );
        assert_eq!(source.query_count(), 1);
        source.close().await;
    }
    let mut reader = native::reader(replies(3, vec![]));
    reader.inner = reader.inner.pause_on_query(7);
    let trace = reader.trace.clone();
    let mut source = BackupHistorySource::with_io(reader, &request(), "fixture.invalid".into(), 1433).unwrap();
    assert!(tokio::time::timeout(std::time::Duration::from_millis(10), source.describe()).await.is_err());
    assert_eq!(source.query_count(), 7);
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
    assert_eq!(trace.closed.load(Ordering::SeqCst), 1);
}
#[tokio::test]
async fn caller_intersection_and_query_ceiling_never_expand_the_manual_unqualified_profile() {
    let requested = CollectionBudget {
        page_rows: 1,
        page_bytes: 4096,
        total_bytes: 8192,
        max_pages: 2,
        max_queries: 10,
        max_snapshot_rows: 2,
        statement_timeout_ms: 500,
        total_timeout_ms: 2000,
        ..Default::default()
    };
    assert_eq!(bounded_budget(&requested), requested);
    let upper = bounded_budget(&CollectionBudget::upper_limits());
    assert_eq!(upper.max_queries, 12 + ReadSession::SESSION_SETUP_QUERIES);
    assert_eq!(upper.max_snapshot_rows, 64);
    assert_eq!(upper.statement_timeout_ms, 1000);
    assert_eq!(upper.total_timeout_ms, 5000);
    let mut limited = request();
    limited.budget.max_queries = 10;
    let (mut source, _) = source(replies(3, vec![]), &limited);
    source.describe().await.unwrap();
    drain(&mut source).await;
    assert_eq!(CollectionStop::from_error(&source.finish().await.unwrap_err()), CollectionStop::QueryLimit);
    assert_eq!(source.query_count(), 10);
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
}
