use super::test_support::*;
use super::*;
use crate::read::{OwnedRead, test_support::Read};
fn budget() -> CollectionBudget {
    crate::jobs::bounded_budget(&CollectionBudget::default())
}
#[tokio::test]
async fn persisted_event_status_last_start_nulls_and_native_clocks_share_one_bounded_catalog_projection() {
    let connector = connector();
    let mut io = reader(vec![event("nightly")]);
    let captured = capture(&mut io, &connector, &budget()).await.unwrap();
    assert_eq!(captured.snapshot.metadata.scheduler_setting, Some(MysqlEventSchedulerSetting::Off));
    let OperationalFact::MysqlScheduledEvent(point) = &captured.snapshot.observations[0].fact else { panic!() };
    assert_eq!(point.status, Some(MysqlEventStatus::Disabled));
    assert_eq!(point.last_started_without_offset, None);
    assert_eq!(point.time_zone.as_deref(), Some("SYSTEM"));
    assert!(point.observed_at.ends_with('Z'));
    assert!(point.created_without_offset.as_ref().is_some_and(|time| !time.ends_with('Z')));
    assert_eq!(io.queries.len(), 8);
    assert_eq!(io.close().await, SourceShutdown::Quiescent);
    let query = &io.queries[4];
    assert_eq!(query.1, vec![Value::from("shop"), Value::from(129_u64)]);
    for private in ["EVENT_DEFINITION", "DEFINER", "EVENT_COMMENT", "mysql.event", "ORDER BY", "COUNT("] {
        assert!(!query.0.contains(private), "{private}");
    }
}
#[tokio::test]
async fn native_permission_or_missing_layout_keeps_original_scheduler_setting_without_event_reads() {
    for denied in [false, true] {
        let mut rows = replies(vec![event("unused")]);
        rows[3] =
            if denied { Err(Error::new(ErrorCode::PermissionDenied, "native catalog denied")) } else { Ok(vec![]) };
        rows.remove(4).expect("remove the event response when catalog layout prevents that query");
        let mut io = Read::with(rows);
        let captured = capture(&mut io, &connector(), &budget()).await.unwrap();
        assert_eq!(captured.snapshot.metadata.scheduler_setting, Some(MysqlEventSchedulerSetting::Off));
        assert!(captured.snapshot.observations.is_empty());
        assert_eq!(captured.snapshot.sources[0].state, culprit_core::SourceState::Unavailable);
        assert_eq!(
            captured.snapshot.sources[0].error_code,
            Some(if denied { ErrorCode::PermissionDenied } else { ErrorCode::Unsupported })
        );
        assert_eq!(io.queries.len(), 7);
        assert!(!io.queries.iter().any(|(sql, _, _)| sql.contains("FROM information_schema.EVENTS")));
        io.close().await;
    }
}
#[tokio::test]
async fn partial_or_unknown_visibility_null_status_and_truncated_rows_never_become_complete_empty_health() {
    for unknown in [false, true] {
        let mut rows =
            replies(if unknown { vec![event("one").with_fields(&[("status", Value::NULL)])] } else { vec![] });
        for index in [1, 6] {
            rows[index] = Ok(vec![context("2026-11-20T12:00:00Z").remove(1).unwrap().remove(0).with_fields(&[
                ("event_visibility", Value::UInt(0)),
                ("observed_at", Value::from(if index == 1 { "2026-11-20T12:00:00Z" } else { "2026-11-20T12:00:02Z" })),
            ])]);
        }
        let mut io = Read::with(rows);
        let captured = capture(&mut io, &connector(), &budget()).await.unwrap();
        assert_eq!(captured.snapshot.sources[0].state, culprit_core::SourceState::Partial);
        assert!(!captured.snapshot.sources[0].complete_scope);
        io.close().await;
    }
    let mut limits = budget();
    limits.max_snapshot_rows = 2;
    let mut io = reader(vec![event("a"), event("b"), event("c")]);
    let captured = capture(&mut io, &connector(), &limits).await.unwrap();
    assert_eq!(captured.snapshot.observations.len(), 2);
    assert_eq!(captured.snapshot.sources[0].stopped, Some(CollectionStop::RowLimit));
    assert!(!captured.snapshot.sources[0].complete_scope);
    io.close().await;
}
#[tokio::test]
async fn duplicate_foreign_malformed_and_late_lookahead_rows_refuse_without_partial_repair() {
    let mut limits = budget();
    limits.max_snapshot_rows = 1;
    for rows in [
        vec![event("a"), event("a")],
        vec![event("a"), event("b").with_fields(&[("event_schema", Value::from("other"))])],
        vec![event("a"), event("b").with_fields(&[("last_started", Value::from("2026-11-20T12:00:00Z"))])],
        vec![event("a"), event("b").with_fields(&[("observed_at", Value::from("2026-11-20T13:00:00Z"))])],
    ] {
        let mut io = reader(rows);
        assert!(capture(&mut io, &connector(), &limits).await.is_err());
        io.close().await;
    }
}
#[tokio::test]
async fn native_member_process_connection_database_role_setting_and_clock_changes_refuse_original_authority() {
    for (index, field, value) in [
        (5, "connection_id", Value::UInt(56)),
        (5, "database_name", Value::from("other")),
        (5, "read_only", Value::UInt(1)),
        (6, "server_uuid", Value::from("bbbbbbbb-bbbb-cccc-dddd-eeeeeeeeeeee")),
        (6, "event_scheduler", Value::from("ON")),
        (6, "observed_at", Value::from("2026-11-20T11:00:00Z")),
        (7, "uptime", Value::UInt(99)),
    ] {
        let mut rows = replies(vec![event("one")]);
        let original = std::mem::replace(&mut rows[index], Ok(vec![])).unwrap().remove(0);
        rows[index] = Ok(vec![original.with_fields(&[(field, value)])]);
        let mut io = Read::with(rows);
        assert!(capture(&mut io, &connector(), &budget()).await.is_err(), "{field}");
        io.close().await;
    }
}
#[tokio::test]
async fn terminal_native_fences_never_continue_into_a_later_source_or_context() {
    for stop in [
        CollectionStop::TimedOut,
        CollectionStop::Cancelled,
        CollectionStop::CancellationUnconfirmed,
        CollectionStop::QueryLimit,
        CollectionStop::ByteLimit,
    ] {
        let mut rows = replies(vec![event("one")]);
        rows[4] = Err(stop.error());
        let mut io = Read::with(rows);
        assert_eq!(CollectionStop::from_error(&capture(&mut io, &connector(), &budget()).await.err().unwrap()), stop);
        assert_eq!(io.queries.len(), 5);
        io.close().await;
    }
}
#[test]
fn native_scheduler_and_replica_statuses_preserve_null_and_unknown_instead_of_a_failure_result() {
    for (native, expected) in [
        ("REPLICA_SIDE_DISABLED", MysqlEventStatus::ReplicaSideDisabled),
        ("SLAVESIDE_DISABLED", MysqlEventStatus::SlavesideDisabled),
        ("NEW_NATIVE_STATE", MysqlEventStatus::Unknown),
    ] {
        let row = event("one").with_fields(&[("status", Value::from(native))]);
        assert_eq!(decode(&row, "shop").unwrap().status, Some(expected));
    }
    assert!(!supported(Flavor::Mariadb, "10.5.25-MariaDB"));
    assert!(supported(Flavor::Mariadb, "11.8.9-MariaDB"));
}

#[tokio::test]
async fn denied_event_rows_and_nullable_configuration_keep_independent_native_context_without_retry() {
    let mut rows = replies(vec![]);
    rows[4] = Err(Error::new(ErrorCode::PermissionDenied, "event catalog denied"));
    let mut io = Read::with(rows);
    let captured = capture(&mut io, &connector(), &budget()).await.unwrap();
    assert!(captured.snapshot.observations.is_empty());
    assert_eq!(captured.snapshot.metadata.scheduler_setting, Some(MysqlEventSchedulerSetting::Off));
    assert_eq!(captured.snapshot.sources[0].error_code, Some(ErrorCode::PermissionDenied));
    assert_eq!(io.queries.len(), 8);
    io.close().await;
    let mut rows = replies(vec![event("known").with_fields(&[("source_scheduler", Value::NULL)])]);
    for index in [1, 6] {
        let original = std::mem::replace(&mut rows[index], Ok(vec![])).unwrap().remove(0);
        rows[index] = Ok(vec![original.with_fields(&[("event_scheduler", Value::NULL)])]);
    }
    let mut io = Read::with(rows);
    let captured = capture(&mut io, &connector(), &budget()).await.unwrap();
    assert_eq!(captured.snapshot.metadata.scheduler_setting, None);
    assert_eq!(captured.snapshot.observations.len(), 1);
    io.close().await;
}
#[tokio::test]
async fn mariadb_uses_its_native_uid_and_status_clock_without_requiring_statement_instrumentation() {
    let mut rows = replies(vec![event("maria").with_fields(&[
        ("status", Value::from("SLAVESIDE_DISABLED")),
        ("source_member", Value::from("AAAAAAAAAAAAAAAAAAAAAAAAAAA=")),
    ])]);
    for index in [0, 5] {
        let original = std::mem::replace(&mut rows[index], Ok(vec![])).unwrap().remove(0);
        rows[index] = Ok(vec![
            original.with_fields(&[("version", Value::from("11.8.9-MariaDB")), ("performance_schema", Value::UInt(0))]),
        ]);
    }
    for index in [1, 6] {
        let original = std::mem::replace(&mut rows[index], Ok(vec![])).unwrap().remove(0);
        rows[index] = Ok(vec![original.with_fields(&[("server_uid", Value::from("AAAAAAAAAAAAAAAAAAAAAAAAAAA="))])]);
    }
    let mut io = Read::with(rows);
    let captured = capture(&mut io, &connector(), &budget()).await.unwrap();
    assert!(captured.snapshot.context.has_qualified_local_owner());
    assert!(io.queries[1].0.contains("@@GLOBAL.server_uid"));
    assert!(!io.queries[1].0.contains("server_uuid"));
    assert!(io.queries[2].0.contains("information_schema.GLOBAL_STATUS"));
    let OperationalFact::MysqlScheduledEvent(event) = &captured.snapshot.observations[0].fact else { unreachable!() };
    assert_eq!(event.status, Some(MysqlEventStatus::SlavesideDisabled));
    io.close().await;
}
#[tokio::test]
async fn retained_encoded_body_obeys_the_callers_byte_cap_without_publishing_a_prefix() {
    let mut limit = budget();
    limit.total_bytes = 1024;
    limit.page_bytes = 1024;
    limit.validate().unwrap();
    let mut io = reader(vec![event("one")]);
    assert_eq!(
        CollectionStop::from_error(&capture(&mut io, &connector(), &limit).await.err().unwrap()),
        CollectionStop::ByteLimit
    );
    io.close().await;
}

#[tokio::test]
async fn actual_data_frame_member_socket_role_or_configuration_change_cannot_borrow_matching_endpoints() {
    for (field, value) in [
        ("source_member", Value::from("bbbbbbbb-bbbb-cccc-dddd-eeeeeeeeeeee")),
        ("source_connection_id", Value::UInt(56)),
        ("source_read_only", Value::UInt(1)),
        ("source_scheduler", Value::from("ON")),
        ("source_time_zone", Value::from("+01:00")),
    ] {
        for row in [event("one"), empty()] {
            let mut io = reader(vec![row.with_fields(&[(field, value.clone())])]);
            assert!(capture(&mut io, &connector(), &budget()).await.is_err(), "{field}");
            io.close().await;
        }
    }
}

#[tokio::test]
async fn zero_native_connection_identity_cannot_authorize_a_catalog_or_fall_back_to_an_empty_result() {
    let mut rows = replies(vec![event("one")]);
    let original = std::mem::replace(&mut rows[0], Ok(vec![])).unwrap().remove(0);
    rows[0] = Ok(vec![original.with_fields(&[("connection_id", Value::UInt(0))])]);
    let mut io = Read::with(rows);
    assert_eq!(
        CollectionStop::from_error(&capture(&mut io, &connector(), &budget()).await.err().unwrap()),
        CollectionStop::SourceUnavailable
    );
    assert_eq!(io.queries.len(), 1);
    io.close().await;
}
