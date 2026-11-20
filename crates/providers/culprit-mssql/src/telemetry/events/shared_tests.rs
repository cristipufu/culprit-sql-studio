use super::*;
use crate::{
    deadlocks::{
        system_health::tests::configuration,
        tests::{KEY_LOOKUP_XDL, event, file_row},
    },
    instance::test_support::{epoch, metadata},
    resources::test_support::{FakeRead, replace},
    value::{Rec, Val},
};

fn request() -> CollectionRequest {
    CollectionRequest {
        subject: CollectionSubject::Shared {
            resource: SharedResourceIdentity {
                engine: culprit_core::Engine::Mssql,
                kind: SharedResourceKind::Instance,
                namespace: NativeIdentity::new("fixture-machine").qualified("kind", "sql-server-network-machine"),
                native: NativeIdentity::new(r"fixture-machine\NATIVE").qualified("kind", "sql-server-instance"),
            },
        },
        selection: CollectionSelection::DeadlockHistory {
            from: "2020-01-01T06:00:00Z".into(),
            until: "2020-01-01T06:01:00Z".into(),
        },
        budget: CollectionBudget { page_rows: 1, ..Default::default() },
    }
}

fn replies(events: Vec<Rec>, verified: Vec<Rec>, final_configuration: Vec<Rec>, final_instance: Rec) -> FakeRead {
    FakeRead::new(vec![
        Ok(vec![metadata()]),
        Ok(vec![epoch(100)]),
        Ok(vec![configuration()]),
        Ok(events),
        Ok(verified),
        Ok(final_configuration),
        Ok(vec![replace(&final_instance, &[("observed_at", Val::Text("2020-01-01T06:03:00Z".into()))])]),
        Ok(vec![epoch(100)]),
    ])
}

fn rows() -> Vec<Rec> {
    let at = "2020-01-01T06:00:01.1234567Z";
    let named = event(at);
    let unnamed = event("2020-01-01T06:00:01.1234568Z").replace(r#" currentdbname="shop""#, "");
    vec![file_row(at, &named), file_row("2020-01-01T06:00:01.1234568Z", &unnamed)]
}

async fn consume(source: &mut EventSource<FakeRead>) -> (CollectionDescription, Vec<TelemetryRecord>) {
    let description = source.describe().await.unwrap();
    let mut cursor = None;
    let mut all = Vec::new();
    loop {
        let CollectionRead::Page { records, next_cursor } =
            source.read_page(TelemetryStream::DeadlockEvents, cursor.as_deref()).await.unwrap()
        else {
            panic!()
        };
        all.extend(records);
        cursor = next_cursor;
        if cursor.is_none() {
            break;
        }
    }
    (description, all)
}

#[tokio::test]
async fn shared_original_source_keeps_named_other_and_unresolved_graphs_without_a_database_owner_or_complete_history() {
    let rows = rows();
    let mut source = EventSource::with_io(
        replies(rows.clone(), rows, vec![configuration()], metadata()),
        &request(),
        "unused-label".into(),
        1433,
    )
    .unwrap();
    assert_eq!(source.query_count(), 0);
    let (description, records) = consume(&mut source).await;
    assert!(matches!(description.target, CollectionTarget::Shared(_)));
    assert!(matches!(description.partition, SourcePartition::InstanceLocal { member: Some(_), epoch: Some(_) }));
    assert!(!CollectionSubject::Database.accepts(&description.target));
    assert_eq!(description.continuity, SourceContinuity::Unverified);
    let history = description.observation.events().unwrap();
    assert!(!history.history_complete && history.retained_from.is_none());
    assert_eq!(records.len(), 2);
    let capture = CaptureIdentity { observation_id: "shared-original-deadlocks".into(), revision: None };
    for record in &records {
        assert!(ExportedRecord::new(&description, &capture, record.clone()).is_ok());
    }
    let TelemetryRecord::Deadlock(first) = &records[0] else { panic!() };
    let TelemetryRecord::Deadlock(second) = &records[1] else { panic!() };
    assert_ne!(first.position, second.position, "fractional native clocks remain event identity");
    assert!(first.processes.iter().any(|process| process.database.as_deref() == Some("shop")));
    assert!(second.processes.iter().all(|process| process.database.is_none()));
    assert!(records.iter().all(|record| {
        match record {
            TelemetryRecord::Deadlock(row) => row
                .processes
                .iter()
                .all(|process| process.login.is_none() && process.host.is_none() && process.application.is_none()),
            _ => false,
        }
    }));
    let completion = source.finish().await.unwrap();
    let shutdown = source.close().await;
    assert_eq!(completion, SourceCompletion::Verified);
    assert_eq!(shutdown, SourceShutdown::Quiescent);
    assert_eq!(source.query_count(), 8);
    assert!(
        source.io.calls.iter().all(|call| !call.sql.contains("sys.dm_xe_sessions")),
        "active emitter state is not retained-file ownership"
    );
    assert_eq!(source.io.closed, 1);
    assert!(source.io.calls.iter().all(|call| !call.sql.contains("dm_xe_session_targets")));
    assert!(source.io.calls.iter().all(|call| !call.sql.contains("dm_xe_database_session_targets")));
}

#[tokio::test]
async fn current_process_and_xe_witness_do_not_relabel_the_generation_of_an_older_rollover_event() {
    let at = "2019-01-01T06:00:01Z";
    let rows = vec![file_row(at, &event(at))];
    let mut request = request();
    request.selection = CollectionSelection::DeadlockHistory {
        from: "2019-01-01T06:00:00Z".into(),
        until: "2019-01-01T06:01:00Z".into(),
    };
    let mut source = EventSource::with_io(
        replies(rows.clone(), rows, vec![configuration()], metadata()),
        &request,
        "unused".into(),
        1433,
    )
    .unwrap();
    let (description, records) = consume(&mut source).await;
    assert_eq!(records.len(), 1);
    let TelemetryRecord::Deadlock(row) = &records[0] else { panic!() };
    assert_eq!(row.at, "2019-01-01T06:00:01.000000000Z");
    assert!(!description.observation.events().unwrap().history_complete);
    assert_eq!(description.continuity, SourceContinuity::Unverified);
    assert_eq!(source.finish().await.unwrap(), SourceCompletion::Verified);
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
}

#[tokio::test]
async fn changed_native_process_or_persistent_file_configuration_prevents_completed_capture() {
    for changed in ["process", "catalog-session", "target", "file", "dropped"] {
        let mut final_configuration = configuration();
        let mut final_instance = metadata();
        match changed {
            "process" => final_instance = replace(&final_instance, &[("process_id", Val::Int(43))]),
            "catalog-session" => {
                final_configuration = replace(&final_configuration, &[("event_session_id", Val::Int(2))])
            }
            "target" => final_configuration = replace(&final_configuration, &[("target_id", Val::Int(2))]),
            "file" => {
                final_configuration = replace(
                    &final_configuration,
                    &[("file_path", Val::Text("/var/opt/mssql/log/another-health.xel".into()))],
                )
            }
            "dropped" => {}
            _ => unreachable!(),
        }
        let final_configuration = if changed == "dropped" { vec![] } else { vec![final_configuration] };
        let mut source = EventSource::with_io(
            replies(rows(), rows(), final_configuration, final_instance),
            &request(),
            "unused".into(),
            1433,
        )
        .unwrap();
        consume(&mut source).await;
        let completion = source.finish().await;
        let shutdown = source.close().await;
        if changed == "dropped" {
            assert_eq!(completion.unwrap_err().code, culprit_core::ErrorCode::Unsupported);
        } else {
            assert_eq!(completion.unwrap(), SourceCompletion::Changed, "{changed}");
        }
        assert_eq!(shutdown, SourceShutdown::Quiescent);
    }
}

#[tokio::test]
async fn denied_or_relative_native_configuration_and_wrong_subject_never_trigger_runtime_target_fallback() {
    let mut source = EventSource::with_io(
        FakeRead::new(vec![
            Ok(vec![metadata()]),
            Ok(vec![epoch(100)]),
            Ok(vec![replace(&configuration(), &[("file_path", Val::Text("system_health.xel".into()))])]),
        ]),
        &request(),
        "unused".into(),
        1433,
    )
    .unwrap();
    assert_eq!(source.describe().await.unwrap_err().code, culprit_core::ErrorCode::Unsupported);
    assert_eq!(source.io.calls.len(), 3);
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
    let mut foreign = request();
    let CollectionSubject::Shared { resource } = &mut foreign.subject else { panic!() };
    resource.native.id = "foreign-native-instance".into();
    let mut source = EventSource::with_io(
        FakeRead::new(vec![Ok(vec![metadata()]), Ok(vec![epoch(100)])]),
        &foreign,
        "unused".into(),
        1433,
    )
    .unwrap();
    assert_eq!(CollectionStop::from_error(&source.describe().await.unwrap_err()), CollectionStop::SourceChanged);
    assert_eq!(source.io.calls.len(), 2);
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
}

#[tokio::test]
async fn unsupported_native_file_timestamp_profiles_stop_before_configuration_or_file_dispatch() {
    for major in ["11", "13"] {
        let mut source = EventSource::with_io(
            FakeRead::new(vec![
                Ok(vec![replace(&metadata(), &[("major", Val::Text(major.into()))])]),
                Ok(vec![epoch(100)]),
            ]),
            &request(),
            "unused".into(),
            1433,
        )
        .unwrap();
        assert_eq!(source.describe().await.unwrap_err().code, culprit_core::ErrorCode::Unsupported);
        assert_eq!(source.io.calls.len(), 2);
        assert_eq!(source.close().await, SourceShutdown::Quiescent);
    }
}

#[tokio::test]
async fn event_overflow_malformed_graphs_and_late_membership_change_never_become_complete_history() {
    let mut bounded = request();
    bounded.budget.max_snapshot_rows = 1;
    let mut source = EventSource::with_io(
        replies(rows(), vec![], vec![configuration()], metadata()),
        &bounded,
        "unused".into(),
        1433,
    )
    .unwrap();
    source.describe().await.unwrap();
    assert_eq!(
        CollectionStop::from_error(&source.read_page(TelemetryStream::DeadlockEvents, None).await.unwrap_err()),
        CollectionStop::RowLimit
    );
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
    let bad = file_row("2020-01-01T06:00:01Z", &KEY_LOOKUP_XDL.replace("</deadlock>", ""));
    let mut source = EventSource::with_io(
        replies(vec![bad], vec![], vec![configuration()], metadata()),
        &request(),
        "unused".into(),
        1433,
    )
    .unwrap();
    source.describe().await.unwrap();
    assert!(source.read_page(TelemetryStream::DeadlockEvents, None).await.is_err());
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
    let mut source = EventSource::with_io(
        replies(vec![rows().remove(0)], rows(), vec![configuration()], metadata()),
        &request(),
        "unused".into(),
        1433,
    )
    .unwrap();
    consume(&mut source).await;
    assert_eq!(source.finish().await.unwrap(), SourceCompletion::Changed);
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
}

#[tokio::test]
async fn cancellation_keeps_the_same_original_reader_and_unconfirmed_close_remains_failure() {
    let io = replies(rows(), vec![], vec![configuration()], metadata()).pause_on_query(4);
    let mut source = EventSource::with_io(io, &request(), "unused".into(), 1433).unwrap();
    source.describe().await.unwrap();
    assert!(
        tokio::time::timeout(
            std::time::Duration::from_millis(10),
            source.read_page(TelemetryStream::DeadlockEvents, None),
        )
        .await
        .is_err()
    );
    assert_eq!(source.io.calls.len(), 4);
    assert_eq!(source.io.closed, 0);
    source.io.shutdown = SourceShutdown::Unconfirmed;
    assert_eq!(source.close().await, SourceShutdown::Unconfirmed);
    assert!(source.read_page(TelemetryStream::DeadlockEvents, None).await.is_err());
    assert_eq!(source.io.calls.len(), 4);
}

#[test]
fn shared_profile_only_intersects_caller_limits_and_never_becomes_a_database_or_pool_subject() {
    let strict = CollectionBudget {
        page_rows: 1,
        max_snapshot_rows: 1,
        page_bytes: 1024,
        total_bytes: 2048,
        max_sql_bytes: 1024,
        max_plan_bytes: 1024,
        max_pages: 1,
        max_queries: 2,
        statement_timeout_ms: 10,
        total_timeout_ms: 20,
    };
    assert_eq!(shared_budget(&strict), strict);
    assert_eq!(shared_budget(&CollectionBudget::upper_limits()).max_queries, 8);
    let mut unsupported = request();
    let CollectionSubject::Shared { resource } = &mut unsupported.subject else { panic!() };
    resource.kind = SharedResourceKind::Pool;
    assert!(EventSource::with_io(FakeRead::new(vec![]), &unsupported, "unused".into(), 1433).is_err());
}

mod native_tests;
