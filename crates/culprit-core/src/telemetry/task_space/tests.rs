use super::*;

fn observation() -> TaskSpaceObservation {
    TaskSpaceObservation {
        source: "native-task-space".into(),
        observed_at: "2026-11-13T10:00:00.2Z".into(),
        task: TaskSpaceIdentity { session_id: "42".into(), request_id: "0".into(), execution_context_id: "0".into() },
        native_database_id: "2".into(),
        bytes_per_page: 8192,
        user_allocated_pages: Some("18446744073709551615".into()),
        user_deallocated_pages: Some("0".into()),
        internal_allocated_pages: None,
        internal_deallocated_pages: Some("9007199254740993".into()),
        request: Some(super::super::activity::tests::request("42")),
        before_check: None,
        after_check: None,
        statement_identity: None,
        plan_identity: None,
    }
}
fn snapshot() -> TaskSpaceSnapshot {
    TaskSpaceSnapshot {
        source: "native-task-space".into(),
        observed_from: "2026-11-13T10:00:00.1Z".into(),
        observed_until: "2026-11-13T10:00:00.9Z".into(),
        observations: vec![observation()],
    }
}
fn checked() -> TaskSpaceObservation {
    let mut row = observation();
    row.before_check =
        Some(TaskRequestCheck { observed_at: "2026-11-13T10:00:00.3Z".into(), state: TaskRequestState::SameStatement });
    row.after_check =
        Some(TaskRequestCheck { observed_at: "2026-11-13T10:00:00.7Z".into(), state: TaskRequestState::SameStatement });
    row.request.as_mut().unwrap().statement =
        DefinitionPayload::sql("SELECT value FROM dbo.orders WHERE id=42", crate::Engine::Mssql);
    row.statement_identity = Some(NativeIdentity::new("native-sql-handle"));
    row.plan_identity = Some(NativeIdentity::new("native-plan-handle"));
    row
}

#[test]
fn task_counts_preserve_exact_values_zero_null_and_native_tuple_without_inventing_task_lifetime() {
    let mut row = observation();
    assert!(row.is_valid());
    assert_eq!(row.request_context(), TaskRequestContext::NativeJoinOnly);
    let key = row.key().unwrap();
    let encoded = serde_json::to_value(&row).unwrap();
    assert_eq!(encoded["user_allocated_pages"], "18446744073709551615");
    assert_eq!(encoded["user_deallocated_pages"], "0");
    assert!(encoded["internal_allocated_pages"].is_null());
    row.user_allocated_pages = Some("1".into());
    row.request.as_mut().unwrap().key.request.as_mut().unwrap().generation = "later-request-context".into();
    assert_eq!(
        row.key().unwrap(),
        key,
        "capture-local tuple does not claim persistent generation or counter continuity"
    );
    row.task.execution_context_id = "1".into();
    assert_ne!(row.key().unwrap(), key);
}

#[test]
fn changed_missing_and_unverified_contexts_preserve_counters_but_cannot_publish_stale_sql_or_handles() {
    for (state, context) in [
        (TaskRequestState::Changed, TaskRequestContext::Changed),
        (TaskRequestState::Missing, TaskRequestContext::Missing),
        (TaskRequestState::Unverified, TaskRequestContext::Unverified),
    ] {
        let mut row = checked();
        row.after_check.as_mut().unwrap().state = state;
        assert_eq!(row.request_context(), context);
        assert!(!row.is_valid());
        row.statement_identity = None;
        row.plan_identity = None;
        assert!(!row.is_valid(), "current SQL requires both matching request checks");
        row.request.as_mut().unwrap().statement =
            DefinitionPayload::Unavailable { reason: PayloadUnavailable::Missing };
        assert!(row.is_valid());
        assert_eq!(row.user_deallocated_pages.as_deref(), Some("0"));
    }
    let row = checked();
    assert_eq!(row.request_context(), TaskRequestContext::CheckedAtBounds);
    assert!(row.is_valid());
}

#[test]
fn associated_request_must_match_task_session_request_and_selected_database_scope() {
    for case in ["session", "request", "scope", "idle", "missing"] {
        let mut row = checked();
        match case {
            "session" => row.request.as_mut().unwrap().key.session.id = "43".into(),
            "request" => row.request.as_mut().unwrap().key.request.as_mut().unwrap().id = "1".into(),
            "scope" => row.request.as_mut().unwrap().scope = ActivityScopeBasis::SessionDatabase,
            "idle" => row.request.as_mut().unwrap().idle = true,
            "missing" => row.request = None,
            _ => unreachable!(),
        }
        assert!(!row.is_valid(), "{case}");
    }
    let row = observation();
    assert!(row.fits_database_capture("sample"));
    assert!(!row.fits_database_capture("another-db"));
}

#[test]
fn source_bounds_and_uniqueness_include_independent_request_check_clocks() {
    assert!(snapshot().is_valid());
    for case in ["source", "row-clock", "before-clock", "after-clock", "reversed-checks", "duplicate"] {
        let mut value = snapshot();
        value.observations[0] = checked();
        match case {
            "source" => value.observations[0].source = "other-source".into(),
            "row-clock" => value.observations[0].observed_at = "2026-11-13T10:00:01Z".into(),
            "before-clock" => {
                value.observations[0].before_check.as_mut().unwrap().observed_at = "2026-11-13T09:59:59Z".into()
            }
            "after-clock" => {
                value.observations[0].after_check.as_mut().unwrap().observed_at = "2026-11-13T10:00:01Z".into()
            }
            "reversed-checks" => {
                value.observations[0].before_check.as_mut().unwrap().observed_at = "2026-11-13T10:00:00.8Z".into()
            }
            "duplicate" => value.observations.push(value.observations[0].clone()),
            _ => unreachable!(),
        }
        assert!(!value.is_valid(), "{case}");
    }
}

#[test]
fn missing_native_measurements_are_not_zero_and_invalid_units_or_counts_are_rejected() {
    let mut row = observation();
    row.user_allocated_pages = None;
    row.user_deallocated_pages = None;
    row.internal_deallocated_pages = None;
    assert!(row.is_valid());
    for invalid in ["-1", "1.5", "18446744073709551616"] {
        row.internal_allocated_pages = Some(invalid.into());
        assert!(!row.is_valid());
    }
    row.internal_allocated_pages = None;
    row.bytes_per_page = 0;
    assert!(!row.is_valid());
    row.bytes_per_page = 8192;
    row.task.session_id = "0".into();
    assert!(!row.is_valid());
}

#[test]
fn numeric_task_identity_aliases_cannot_duplicate_one_native_tuple_or_hide_a_zero_session() {
    for field in ["session", "zero-session", "request", "execution", "database"] {
        let mut row = observation();
        match field {
            "session" => row.task.session_id = "042".into(),
            "zero-session" => row.task.session_id = "00".into(),
            "request" => row.task.request_id = "00".into(),
            "execution" => row.task.execution_context_id = "00".into(),
            "database" => row.native_database_id = "02".into(),
            _ => unreachable!(),
        }
        assert!(!row.is_valid(), "{field}");
    }
}

#[test]
fn ordinary_task_redaction_preserves_native_client_context_and_redacts_only_qualified_sql() {
    let mut row = checked();
    row.request.as_mut().unwrap().client =
        Some(ActivityClient { application: Some("native-label".into()), user: None, host: None });
    row.redact(crate::Engine::Mssql);
    assert_eq!(row.request.as_ref().unwrap().client.as_ref().unwrap().application.as_deref(), Some("native-label"));
    assert!(
        matches!(&row.request.as_ref().unwrap().statement, DefinitionPayload::Available {text,..} if !text.contains("42"))
    );
    assert!(row.is_valid());
}

#[test]
fn task_outer_source_and_key_are_bound_and_payload_inventory_points_to_the_exact_nested_statement() {
    let row = checked();
    let observation = OperationalObservation {
        source: row.source.clone(),
        key: row.key().unwrap(),
        fact: OperationalFact::TaskSpace(Box::new(row)),
    };
    assert!(observation.is_valid());
    let mut wrong = observation.clone();
    wrong.source = "different-native-source".into();
    assert!(!wrong.is_valid());
    let mut wrong = observation.clone();
    wrong.key = "another-task-key".into();
    assert!(!wrong.is_valid());
    let record = TelemetryRecord::Operational(observation);
    let fields = record.definition_payloads().collect::<Vec<_>>();
    assert_eq!(fields.len(), 1);
    let serialized = serde_json::json!({"record":&record});
    assert_eq!(serialized.pointer(fields[0].pointer).unwrap(), &serde_json::to_value(fields[0].payload).unwrap());
}

#[test]
fn snapshot_sharing_sanitizes_task_client_and_sql_without_changing_collected_counter_evidence() {
    let mut scan: crate::Snapshot =
        serde_json::from_str(include_str!("../../../../../fixtures/mssql-demo.json")).unwrap();
    let mut row = checked();
    let request = row.request.as_mut().unwrap();
    let sql = "SELECT value FROM dbo.orders WHERE secret=N'task-private-canary'";
    request.statement = DefinitionPayload::Available {
        text: sql.into(),
        sha256: digest(sql.as_bytes()),
        redaction_version: REDACTION_VERSION,
    };
    request.client = Some(ActivityClient {
        application: Some("task-private-app-canary".into()),
        user: Some("task-private-user-canary".into()),
        host: None,
    });
    let mut evidence = snapshot();
    evidence.observations = vec![row.clone()];
    scan.operations.task_space = Some(evidence);
    let shared = crate::redact::redact_snapshot(&scan);
    let task = &shared.operations.task_space.as_ref().unwrap().observations[0];
    assert!(!serde_json::to_string(&shared).unwrap().contains("-canary"));
    assert!(task.request.as_ref().unwrap().client.is_none());
    assert_eq!(task.task, row.task);
    assert_eq!(task.user_allocated_pages, row.user_allocated_pages);
    assert!(task.is_valid());
    assert_eq!(scan.operations.task_space.as_ref().unwrap().observations[0], row);
    assert_eq!(
        serde_json::to_value(&shared).unwrap(),
        serde_json::to_value(crate::redact::redact_snapshot(&shared)).unwrap()
    );
}
