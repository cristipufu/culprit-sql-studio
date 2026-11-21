use super::*;
use crate::{read::QueryRead, rows::Row};
use std::collections::VecDeque;

struct Read {
    replies: VecDeque<Result<Vec<Row>>>,
    calls: Vec<(String, usize)>,
}
#[async_trait::async_trait]
impl QueryRead for Read {
    async fn query(&mut self, sql: &str, limit: usize) -> Result<Vec<Row>> {
        self.calls.push((sql.into(), limit));
        self.replies.pop_front().expect("only the bounded baseline native reads")
    }
}
fn metadata(at: &str, started: &str, visible: bool) -> Row {
    metadata_with_role(at, started, visible, Some(Some("f")))
}
fn metadata_with_role(at: &str, started: &str, visible: bool, standby: Option<Option<&str>>) -> Row {
    let mut fields = [
        ("build", Some("PostgreSQL 18.6".into())),
        ("database_oid", Some("42".into())),
        ("database", Some("shop".into())),
        ("version", Some("180006".into())),
        ("block_bytes", Some("8192".into())),
        ("started_at", Some(started.into())),
        ("observed_at", Some(at.into())),
        ("full_visibility", Some(if visible { "t" } else { "f" }.into())),
        ("replay_pause_requested_access", Some("f".into())),
        ("replay_pause_state_access", Some("f".into())),
        ("pgss_schema", None),
        ("pgss_columns", None),
        ("pgss_info", Some("f".into())),
    ]
    .into_iter()
    .map(|(name, value)| (name.into(), value))
    .collect::<Vec<_>>();
    if let Some(standby) = standby {
        fields.push(("standby".into(), standby.map(str::to_owned)));
    }
    Row::from_fields(fields)
}
fn actor(pid: &str, state: &str, backend: Option<&str>) -> Row {
    actor_with_wait(pid, state, backend, Some(("Lock", "transactionid")), "100")
}
fn member(address: Option<&str>, at: &str) -> Row {
    Row::from_fields(
        [
            ("address", address.map(str::to_owned)),
            ("port", Some("5432".into())),
            ("system_identifier", Some("42".into())),
            ("observed_at", Some(at.into())),
        ]
        .map(|(name, value)| (name.into(), value)),
    )
}
fn actor_with_wait(pid: &str, state: &str, backend: Option<&str>, wait: Option<(&str, &str)>, elapsed_ms: &str) -> Row {
    participant(pid, state, backend, wait, elapsed_ms, Some("client backend"))
}
fn participant(
    pid: &str,
    state: &str,
    backend: Option<&str>,
    wait: Option<(&str, &str)>,
    elapsed_ms: &str,
    backend_type: Option<&str>,
) -> Row {
    Row::from_fields(
        [
            ("observed_at", Some("2020-01-01T06:00:00.100000Z".into())),
            ("pid", Some(pid.into())),
            ("backend_start", backend.map(str::to_owned)),
            ("query_start", Some("2020-01-01T06:00:00Z".into())),
            ("transaction_start", None),
            ("database_oid", Some("42".into())),
            ("backend_type", backend_type.map(str::to_owned)),
            ("database", Some("shop".into())),
            ("state", Some(state.into())),
            ("wait_event_type", wait.map(|(kind, _)| kind.into())),
            ("wait_event", wait.map(|(_, event)| event.into())),
            ("elapsed_ms", Some(elapsed_ms.into())),
            ("transaction_ms", None),
            ("idle_ms", Some("0".into())),
            ("query_id", None),
            ("application_name", Some("app".into())),
        ]
        .map(|(name, value)| (name.into(), value)),
    )
}
fn request(limit: u32) -> CollectionRequest {
    CollectionRequest {
        subject: CollectionSubject::Database,
        selection: CollectionSelection::Activity,
        budget: CollectionBudget { max_snapshot_rows: limit, ..Default::default() },
    }
}
fn reader(rows: Vec<Row>, visible: bool) -> Read {
    Read {
        replies: vec![
            Ok(vec![metadata("2020-01-01T06:00:00Z", "2020-01-01T05:00:00Z", visible)]),
            Ok(vec![member(Some("192.0.2.8"), "2020-01-01T06:00:00.050000Z")]),
            Ok(rows),
            Ok(vec![metadata("2020-01-01T06:00:00.200000Z", "2020-01-01T05:00:00Z", visible)]),
            Ok(vec![member(Some("192.0.2.8"), "2020-01-01T06:00:00.250000Z")]),
        ]
        .into(),
        calls: vec![],
    }
}
#[tokio::test]
async fn metadata_baseline_keeps_short_waits_and_ordinary_backends_before_any_optional_reads() {
    let mut reader = reader(
        vec![actor("11", "active", Some("2020-01-01T05:30:00Z")), actor("12", "idle", Some("2020-01-01T05:31:00Z"))],
        true,
    );
    let capture = capture(&mut reader, "fixture.invalid", 5432, &request(500)).await.unwrap();
    let native = capture.description.observation.activity().unwrap();
    assert!(native.is_valid() && native.coverage.complete_scope);
    assert_eq!(native.observations, 2);
    assert_eq!(native.population, ActivityPopulation::ClientBackends);
    assert_eq!(reader.calls.len(), 5);
    assert_eq!(reader.calls.iter().filter(|(sql, _)| sql.contains("pg_control_system")).count(), 2);
    let context = NativeReadContext::from_activity(&capture.description).unwrap();
    assert!(context.has_qualified_local_owner());
    assert_eq!(context.observed_from, "2020-01-01T06:00:00.000000000Z");
    assert_eq!(context.observed_until, "2020-01-01T06:00:00.250000000Z");
    assert!(reader.calls.iter().all(|(sql, _)| !sql.contains("pg_blocking_pids")
        && !sql.contains("pg_locks")
        && !sql.contains("a.query ELSE")));
    let TelemetryRecord::Activity(row) = &capture.records[0] else { panic!() };
    assert_eq!(row.elapsed_ms.as_deref(), Some("100"));
    assert_eq!(row.wait.as_ref().unwrap().native_type.as_deref(), Some("Lock: transactionid"));
    assert!(row.wait.as_ref().unwrap().owners.is_empty());
    assert_eq!(row.blocking_status, ActivityBlockingStatus::Unknown);
    assert_eq!(row.waiting_on_lock, Some(true));
    assert!(row.statement_identity.is_some());
    assert!(matches!(row.statement, DefinitionPayload::Unavailable { reason: PayloadUnavailable::NotCollected }));
}
#[tokio::test]
async fn a_native_lock_point_at_forty_seconds_needs_neither_an_owner_walk_nor_an_invented_wait_duration() {
    let row = actor_with_wait("11", "active", Some("2020-01-01T05:30:00Z"), Some(("Lock", "transactionid")), "40000");
    let mut reader = reader(vec![row], true);
    let capture = capture(&mut reader, "fixture.invalid", 5432, &request(500)).await.unwrap();
    let TelemetryRecord::Activity(row) = &capture.records[0] else { panic!() };
    assert_eq!(row.waiting_on_lock, Some(true));
    assert_eq!(row.elapsed_ms.as_deref(), Some("40000"));
    assert!(row.wait.as_ref().unwrap().elapsed_ms.is_none());
    assert!(row.wait.as_ref().unwrap().owners.is_empty());
    assert_eq!(row.blocking_status, ActivityBlockingStatus::Unknown);
    assert!(!row.conditions(&ActivityPolicy::default()).contains(&ActivityCondition::Blocked));
    assert_eq!(reader.calls.len(), 5);
    assert!(reader.calls.iter().all(|(sql, _)| !sql.contains("pg_blocking_pids")
        && !sql.contains("pg_locks")
        && !sql.contains("a.query ELSE")));
}

#[tokio::test]
async fn unknown_extension_and_other_native_waits_are_not_a_proven_negative_lock_inventory() {
    for wait in [None, Some(("Extension", "DocDBWait")), Some(("IO", "DataFileRead")), Some(("Client", "ClientRead"))] {
        let row = actor_with_wait("11", "active", Some("2020-01-01T05:30:00Z"), wait, "40000");
        let mut reader = reader(vec![row], true);
        let capture = capture(&mut reader, "fixture.invalid", 5432, &request(500)).await.unwrap();
        let TelemetryRecord::Activity(row) = &capture.records[0] else { panic!() };
        assert_eq!(row.waiting_on_lock, None, "{wait:?}");
        assert!(row.wait.as_ref().is_none_or(|wait| wait.elapsed_ms.is_none()));
        assert_eq!(reader.calls.len(), 5);
    }
}
#[tokio::test]
async fn a_drained_row_witness_retains_the_prefix_with_partial_native_coverage() {
    let mut reader = reader(
        vec![actor("11", "active", Some("2020-01-01T05:30:00Z")), actor("12", "active", Some("2020-01-01T05:31:00Z"))],
        true,
    );
    let capture = capture(&mut reader, "fixture.invalid", 5432, &request(1)).await.unwrap();
    let native = capture.description.observation.activity().unwrap();
    assert!(native.is_valid());
    assert_eq!(capture.records.len(), 1);
    assert_eq!(native.native_rows, 2);
    assert_eq!(native.coverage.stopped, Some(CollectionStop::RowLimit));
    assert!(!native.coverage.complete_scope);
    assert_eq!(reader.calls[2].1, 2);
    assert_eq!(reader.calls.len(), 5, "only the bounded database/member checks bracket the baseline");
}
#[tokio::test]
async fn hidden_or_disabled_rows_cannot_become_fabricated_idle_sessions_or_complete_absence() {
    for state in ["client backend", "disabled"] {
        let mut reader =
            reader(vec![actor("11", state, None), actor("12", "active", Some("2020-01-01T05:31:00Z"))], false);
        let capture = capture(&mut reader, "fixture.invalid", 5432, &request(500)).await.unwrap();
        let native = capture.description.observation.activity().unwrap();
        assert_eq!(capture.records.len(), 1);
        assert_eq!(native.native_rows, 2);
        assert_eq!(native.coverage.error_code, Some(ErrorCode::PermissionDenied));
        assert_eq!(native.coverage.state, culprit_core::SourceState::Partial);
        assert!(!native.coverage.complete_scope);
        let TelemetryRecord::Activity(visible) = &capture.records[0] else { panic!() };
        assert_eq!(visible.waiting_on_lock, Some(true), "hidden peers cannot erase the visible native positive");
    }
}
#[tokio::test]
async fn final_source_change_and_terminal_failures_withhold_the_frame_without_optional_followup() {
    for final_read in [
        Ok(vec![metadata("2020-01-01T06:00:00.200000Z", "2020-01-01T05:01:00Z", true)]),
        Err(CollectionStop::Cancelled.error()),
        Err(CollectionStop::CancellationUnconfirmed.error()),
    ] {
        let mut reader = reader(vec![actor("11", "active", Some("2020-01-01T05:30:00Z"))], true);
        reader.replies[3] = final_read;
        assert!(capture(&mut reader, "fixture.invalid", 5432, &request(500)).await.is_err());
        assert_eq!(reader.calls.len(), 4);
        assert_eq!(reader.replies.len(), 1, "no member, closure or payload read after a terminal metadata failure");
    }
}

#[tokio::test]
async fn native_member_process_and_updateability_are_retained_from_both_original_read_bounds() {
    for (standby, writable) in [("f", true), ("t", false)] {
        let mut reader = reader(vec![actor("11", "active", Some("2020-01-01T05:30:00Z"))], true);
        for phase in [0, 3] {
            reader.replies[phase] = Ok(vec![metadata_with_role(
                if phase == 0 { "2020-01-01T06:00:00Z" } else { "2020-01-01T06:00:00.200000Z" },
                "2020-01-01T05:00:00Z",
                true,
                Some(Some(standby)),
            )]);
        }
        let capture = capture(&mut reader, "fixture.invalid", 5432, &request(500)).await.unwrap();
        let native = capture.description.observation.activity().unwrap();
        assert_eq!(native.database_writable, Some(writable));
        let SourcePartition::InstanceLocal { member: Some(member), epoch: Some(epoch) } =
            &capture.description.partition
        else {
            panic!("the exact native member and process must survive the original acquisition")
        };
        assert_eq!(member.native.id, "192.0.2.8");
        assert_eq!(member.namespace.id, "42");
        assert_eq!(native.epoch.as_ref(), Some(epoch));
        assert!(native.epoch.is_some());
        assert!(capture.description.matches_request(&request(500)));
        let context = NativeReadContext {
            target: capture.description.target.clone(),
            partition: capture.description.partition.clone(),
            observed_from: native.observed_from.clone(),
            observed_until: native.observed_until.clone(),
            database_writable: native.database_writable,
        };
        assert!(context.has_qualified_local_owner());
        assert_eq!(reader.calls.len(), 5);
        assert!(reader.replies.is_empty());
        assert!(reader.calls.iter().all(|(sql, _)| !sql.contains("pg_blocking_pids") && !sql.contains("pg_locks")));
        let canonical = ExportedRecord::new(
            &capture.description,
            &CaptureIdentity { observation_id: "native-role".into(), revision: None },
            capture.records[0].clone(),
        )
        .unwrap();
        assert!(canonical.record.is_valid());
    }
}

#[tokio::test]
async fn hidden_activity_retains_independent_native_owner_without_claiming_complete_population() {
    let mut reader = reader(vec![actor("11", "active", Some("2020-01-01T05:30:00Z"))], false);
    let capture = capture(&mut reader, "fixture.invalid", 5432, &request(500)).await.unwrap();
    let native = capture.description.observation.activity().unwrap();
    assert_eq!(native.database_writable, Some(true));
    assert_eq!(native.coverage.state, culprit_core::SourceState::Partial);
    assert_eq!(native.coverage.error_code, Some(ErrorCode::PermissionDenied));
    assert!(!native.coverage.complete_scope);
    assert!(NativeReadContext::from_activity(&capture.description).unwrap().has_qualified_local_owner());
    assert_eq!(capture.records.len(), 1);
    assert_eq!(reader.calls.len(), 5);
}

#[tokio::test]
async fn missing_null_or_malformed_native_role_never_defaults_to_primary_or_triggers_fallback_queries() {
    for role in [None, Some(None), Some(Some("unknown")), Some(Some(""))] {
        for phase in [0, 3] {
            let mut reader = reader(vec![actor("11", "active", Some("2020-01-01T05:30:00Z"))], true);
            reader.replies[phase] = Ok(vec![metadata_with_role(
                if phase == 0 { "2020-01-01T06:00:00Z" } else { "2020-01-01T06:00:00.200000Z" },
                "2020-01-01T05:00:00Z",
                true,
                role,
            )]);
            let error = capture(&mut reader, "fixture.invalid", 5432, &request(500)).await.err().unwrap();
            let expected = if phase == 0 { CollectionStop::InvalidPage } else { CollectionStop::SourceChanged };
            assert_eq!(CollectionStop::from_error(&error), expected, "{role:?} at {phase}");
            assert_eq!(reader.calls.len(), phase + 1, "no read or role fallback after invalid native metadata");
        }
    }
}

#[tokio::test]
async fn promotion_or_recovery_role_flip_on_same_process_retires_all_provisional_baseline_rows() {
    for (before, after) in [("f", "t"), ("t", "f")] {
        let mut reader = reader(vec![actor("11", "active", Some("2020-01-01T05:30:00Z"))], true);
        for (phase, role) in [(0, before), (3, after)] {
            reader.replies[phase] = Ok(vec![metadata_with_role(
                if phase == 0 { "2020-01-01T06:00:00Z" } else { "2020-01-01T06:00:00.200000Z" },
                "2020-01-01T05:00:00Z",
                true,
                Some(Some(role)),
            )]);
        }
        let error = capture(&mut reader, "fixture.invalid", 5432, &request(500)).await.err().unwrap();
        assert_eq!(CollectionStop::from_error(&error), CollectionStop::SourceChanged);
        assert_eq!(reader.calls.len(), 4);
        assert_eq!(reader.replies.len(), 1);
    }
}

#[tokio::test]
async fn denied_or_unavailable_initial_native_identity_stays_partial_after_a_later_success() {
    for initial in [
        Err(Error::new(ErrorCode::PermissionDenied, culprit_core::t!("collection.invalid_source"))),
        Ok(vec![member(None, "2020-01-01T06:00:00.050000Z")]),
        Ok(vec![member(Some("not-a-native-address"), "2020-01-01T06:00:00.050000Z")]),
    ] {
        let mut reader = reader(vec![actor("11", "active", Some("2020-01-01T05:30:00Z"))], true);
        reader.replies[1] = initial;
        let captured = capture(&mut reader, "a-configured-route-is-not-a-member", 5432, &request(500)).await.unwrap();
        let frame = captured.description.observation.activity().unwrap();
        let context = NativeReadContext::from_activity(&captured.description).unwrap();
        assert!(!context.has_qualified_local_owner());
        assert!(matches!(context.partition, SourcePartition::InstanceLocal { member: None, epoch: Some(_) }));
        assert_eq!(frame.coverage.state, culprit_core::SourceState::Partial);
        assert_eq!(frame.coverage.stopped, Some(CollectionStop::SourceUnavailable));
        assert!(!frame.coverage.complete_scope);
        assert_eq!(captured.records.len(), 1, "the accessible native lock point remains useful context");
        assert_eq!(reader.calls.len(), 5);
        let exported = ExportedRecord::new(
            &captured.description,
            &CaptureIdentity { observation_id: "unknown-original-member".into(), revision: None },
            captured.records[0].clone(),
        )
        .unwrap();
        assert!(exported.record.is_valid());
    }
}

#[tokio::test]
async fn changed_or_lost_final_member_and_reversed_native_clocks_withhold_all_provisional_rows() {
    for (phase, final_read, expected) in [
        (4, Ok(vec![member(Some("192.0.2.9"), "2020-01-01T06:00:00.250000Z")]), CollectionStop::SourceChanged),
        (
            4,
            Err(Error::new(ErrorCode::PermissionDenied, culprit_core::t!("collection.invalid_source"))),
            CollectionStop::SourceChanged,
        ),
        (4, Ok(vec![member(Some("192.0.2.8"), "2020-01-01T05:59:59Z")]), CollectionStop::SourceChanged),
        (4, Err(CollectionStop::Cancelled.error()), CollectionStop::Cancelled),
        (4, Err(CollectionStop::CancellationUnconfirmed.error()), CollectionStop::CancellationUnconfirmed),
        (3, Ok(vec![metadata("2020-01-01T05:59:59Z", "2020-01-01T05:00:00Z", true)]), CollectionStop::SourceChanged),
    ] {
        let mut reader = reader(vec![actor("11", "active", Some("2020-01-01T05:30:00Z"))], true);
        reader.replies[phase] = final_read;
        let error = capture(&mut reader, "fixture.invalid", 5432, &request(500)).await.err().unwrap();
        assert_eq!(CollectionStop::from_error(&error), expected);
        assert_eq!(reader.calls.len(), phase + 1);
    }
}

#[tokio::test]
async fn lost_final_statistics_visibility_retains_the_owner_and_positive_rows_with_partial_coverage() {
    let mut reader = reader(vec![actor("11", "active", Some("2020-01-01T05:30:00Z"))], true);
    reader.replies[3] = Ok(vec![metadata("2020-01-01T06:00:00.200000Z", "2020-01-01T05:00:00Z", false)]);
    let captured = capture(&mut reader, "fixture.invalid", 5432, &request(500)).await.unwrap();
    let frame = captured.description.observation.activity().unwrap();
    assert!(NativeReadContext::from_activity(&captured.description).unwrap().has_qualified_local_owner());
    assert_eq!(frame.coverage.error_code, Some(ErrorCode::PermissionDenied));
    assert_eq!(frame.coverage.state, culprit_core::SourceState::Partial);
    assert!(!frame.coverage.complete_scope);
    assert_eq!(captured.records.len(), 1);
}

#[tokio::test]
async fn acquired_backend_oid_type_and_utc_start_survive_the_existing_five_reads_without_extra_discovery() {
    let mut reader = reader(vec![actor("11", "idle", Some("2020-01-01T05:30:00Z"))], true);
    let captured = capture(&mut reader, "fixture.invalid", 5432, &request(500)).await.unwrap();
    let TelemetryRecord::Activity(row) = &captured.records[0] else { panic!() };
    let Some(SessionNativeContext::Postgres(native)) = row.session_context.as_deref().unwrap().native.as_deref() else {
        panic!()
    };
    assert_eq!(native.database_oid.as_deref(), Some("42"));
    assert_eq!(native.database_name.as_deref(), Some("shop"));
    assert!(native.is_client_backend());
    assert_eq!(row.key.session.generation, "2020-01-01T05:30:00Z");
    assert!(row.session_counters.logical_reads.is_none());
    assert_eq!(reader.calls.len(), 5);
    assert!(reader.calls[2].0.contains("a.backend_type"));
}

#[tokio::test]
async fn native_parallel_internal_or_unknown_backend_types_remain_original_participants_without_client_authority() {
    for kind in [None, Some("parallel worker"), Some("autovacuum worker"), Some("yb-conn-mgr worker connection")] {
        let point = participant("11", "active", Some("2020-01-01T05:30:00Z"), None, "100", kind);
        let native = activity::decode_row(point.clone(), "42").unwrap().facts.unwrap();
        let Some(SessionNativeContext::Postgres(context)) =
            native.session_context.as_deref().unwrap().native.as_deref()
        else {
            panic!()
        };
        assert_eq!(context.backend_type.as_deref(), kind);
        assert!(!context.is_client_backend());
        let mut reader = reader(vec![point], true);
        let failure = capture(&mut reader, "fixture.invalid", 5432, &request(500)).await.err().unwrap();
        assert_eq!(failure.detail, Some(culprit_core::ErrorDetail::CollectionStopped(CollectionStop::SourceChanged)));
        assert_eq!(reader.calls.len(), 3, "a client-only selection cannot publish contradicting native kinds");
    }
}

#[tokio::test]
async fn client_only_selected_database_frame_refuses_foreign_or_unknown_native_backend_database() {
    for oid in [Some("43"), None] {
        let row = Row::from_fields(
            [
                ("observed_at", Some("2020-01-01T06:00:00.100000Z")),
                ("pid", Some("11")),
                ("backend_start", Some("2020-01-01T05:30:00Z")),
                ("query_start", Some("2020-01-01T06:00:00Z")),
                ("state", Some("active")),
                ("database_oid", oid),
                ("database", Some("native-foreign-label")),
                ("backend_type", Some("client backend")),
            ]
            .map(|(key, value)| (key.into(), value.map(str::to_owned))),
        );
        let native = activity::decode_row(row.clone(), "42").unwrap().facts.unwrap();
        let Some(SessionNativeContext::Postgres(context)) =
            native.session_context.as_deref().unwrap().native.as_deref()
        else {
            panic!()
        };
        assert_eq!(context.database_oid.as_deref(), oid);
        assert_eq!(native.scope, ActivityScopeBasis::OtherDatabase);
        let mut reader = reader(vec![row], true);
        let error = capture(&mut reader, "fixture.invalid", 5432, &request(500)).await.err().unwrap();
        assert_eq!(error.detail, Some(culprit_core::ErrorDetail::CollectionStopped(CollectionStop::SourceChanged)));
        assert_eq!(reader.calls.len(), 3);
    }
}
