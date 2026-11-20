use super::*;

fn current() -> TaskSpaceObservation {
    let mut row = measured_zero();
    row.internal_allocated_pages = Some("9007199254740993".into());
    checked_context(&mut row);
    row
}

#[test]
fn original_current_request_keeps_its_check_interval_separate_from_task_counters() {
    let row = current();
    let original = row.clone();
    let report = explain(&[&row], Some(&source(SourceState::Partial))).unwrap();
    let context = report.entries[0].current_request.as_ref().unwrap();
    assert_eq!(context.request, row.request.as_ref().unwrap().key);
    assert_eq!(context.observed_from, row.before_check.as_ref().unwrap().observed_at);
    assert_eq!(context.observed_until, row.after_check.as_ref().unwrap().observed_at);
    assert_ne!(context.observed_from, row.observed_at);
    assert_ne!(context.observed_until, row.observed_at);
    assert_eq!(context.client, row.request.as_ref().unwrap().client);
    assert_eq!(report.entries[0].activity, TaskAllocationActivity::InternalObjects);
    assert_eq!(report.entries[0].investigation, t!("view.task_space.investigate.internal"));
    assert!(report.limitations.contains(&t!("view.task_space.limit.source_partial")));
    assert_eq!(row, original);
}

#[test]
fn changed_missing_and_unverified_checks_withhold_current_sql_without_erasing_allocations() {
    for state in [TaskRequestState::Changed, TaskRequestState::Missing, TaskRequestState::Unverified] {
        let mut row = current();
        row.after_check.as_mut().unwrap().state = state;
        row.request.as_mut().unwrap().statement =
            DefinitionPayload::Unavailable { reason: PayloadUnavailable::Missing };
        row.statement_identity = None;
        let entry = entry(&row);
        assert!(entry.current_request.is_none());
        assert_eq!(entry.activity, TaskAllocationActivity::InternalObjects);
        assert_eq!(row.internal_allocated_pages.as_deref(), Some("9007199254740993"));
    }
    let mut row = current();
    row.before_check = None;
    row.after_check = None;
    row.statement_identity = None;
    row.request.as_mut().unwrap().statement =
        DefinitionPayload::Unavailable { reason: PayloadUnavailable::NotCollected };
    assert!(entry(&row).current_request.is_none(), "a native join is not a checked current statement");
}

#[test]
fn available_sql_requires_the_same_original_and_checked_native_statement_token() {
    for identity in [None, Some(NativeIdentity::new("another-native-statement"))] {
        let mut row = current();
        row.statement_identity = identity;
        assert!(row.is_valid());
        assert!(entry(&row).current_request.is_none());
    }
    let mut row = current();
    row.request.as_mut().unwrap().statement_identity = None;
    assert!(entry(&row).current_request.is_none());
    let mut row = current();
    row.request.as_mut().unwrap().key.request.as_mut().unwrap().id = "another-request".into();
    assert!(explain(&[&row], None).is_err(), "a current context cannot reattach by session alone");
}

fn peer(row: &TaskSpaceObservation, lifecycle: SessionLifecycle) -> TaskSpaceObservation {
    let mut peer = row.clone();
    peer.task.execution_context_id = "2".into();
    peer.task.request_id = "1".into();
    let request = peer.request.as_mut().unwrap();
    request.key.request.as_mut().unwrap().id = "1".into();
    request.session_context = Some(Box::new(ActivitySessionContext {
        lifecycle,
        login_age_ms: None,
        last_request_end_age_ms: None,
        client_library: None,
        client_process_id: None,
        native: None,
    }));
    assert!(peer.is_valid());
    peer
}

#[test]
fn explicit_negative_generation_peers_withdraw_context_in_both_orders_without_becoming_task_identity() {
    let row = current();
    for lifecycle in [SessionLifecycle::Connecting, SessionLifecycle::PoolResetPrelogin] {
        let peer = peer(&row, lifecycle);
        for observations in [[&row, &peer], [&peer, &row]] {
            let report = explain(&observations, Some(&source(SourceState::Partial))).unwrap();
            assert!(report.entries.iter().all(|entry| entry.current_request.is_none()));
            assert!(report.entries.iter().all(|entry| entry.activity == TaskAllocationActivity::InternalObjects));
        }
    }
    for lifecycle in [SessionLifecycle::Running, SessionLifecycle::Unknown] {
        let peer = peer(&row, lifecycle);
        assert!(explain(&[&row, &peer], None).unwrap().entries.iter().all(|entry| entry.current_request.is_some()));
    }
    let mut foreign_generation = peer(&row, SessionLifecycle::Connecting);
    foreign_generation.request.as_mut().unwrap().key.session.generation = "another-login".into();
    let report = explain(&[&row, &foreign_generation], None).unwrap();
    assert!(report.entries[0].current_request.is_some());
    assert!(report.entries[1].current_request.is_none());
    let mut worker = row.clone();
    worker.task.execution_context_id = "3".into();
    let report = explain(&[&row, &worker], None).unwrap();
    assert!(
        report.entries.iter().all(|entry| entry.current_request.is_some()),
        "parallel workers may repeat a request key"
    );
}

#[test]
fn original_unavailable_statement_reasons_survive_checked_context_without_replacement_sql() {
    for reason in [
        PayloadUnavailable::Missing,
        PayloadUnavailable::Restricted,
        PayloadUnavailable::Encrypted,
        PayloadUnavailable::Oversized,
        PayloadUnavailable::RedactionFailed,
        PayloadUnavailable::NotCollected,
        PayloadUnavailable::SourceUnavailable,
    ] {
        let mut row = current();
        row.request.as_mut().unwrap().statement = DefinitionPayload::Unavailable { reason };
        row.statement_identity = None;
        assert!(row.is_valid());
        let entry = entry(&row);
        assert_eq!(entry.current_request.unwrap().statement, DefinitionPayload::Unavailable { reason });
    }
}

#[test]
fn sql_is_redacted_again_without_mutating_the_original_point_or_choosing_a_cause_from_the_statement() {
    let mut row = current();
    let raw = "SELECT current_task_context FROM dbo.orders WHERE note=N'current-secret-canary' AND id=9264";
    row.request.as_mut().unwrap().statement = DefinitionPayload::Available {
        text: raw.into(),
        sha256: digest(raw.as_bytes()),
        redaction_version: REDACTION_VERSION,
    };
    assert!(row.is_valid());
    let original = row.clone();
    let report = explain(&[&row], None).unwrap();
    let encoded = serde_json::to_string(&report).unwrap();
    assert!(encoded.contains("current_task_context"));
    assert!(!encoded.contains("current-secret-canary") && !encoded.contains("9264"));
    let mut other = row.clone();
    other.request.as_mut().unwrap().statement =
        DefinitionPayload::sql("ALTER INDEX ix_orders ON dbo.orders REBUILD", culprit_core::Engine::Mssql);
    let other = entry(&other);
    assert_eq!(other.activity, report.entries[0].activity);
    assert_eq!(other.investigation, report.entries[0].investigation);
    assert_eq!(other.verify, report.entries[0].verify);
    assert_eq!(row, original);
}

#[test]
fn the_derived_context_is_required_nullable_and_roundtrips_without_native_receipt_or_module_fields() {
    let entry = entry(&current());
    let value = serde_json::to_value(&entry).unwrap();
    assert_eq!(serde_json::from_value::<TaskSpaceExplanation>(value.clone()).unwrap(), entry);
    let mut absent = value.clone();
    absent.as_object_mut().unwrap().remove("current_request");
    assert!(serde_json::from_value::<TaskSpaceExplanation>(absent).is_err());
    let context = &value["current_request"];
    for field in ["module", "allocator", "plan_identity", "statement_identity", "receipt", "native_properties"] {
        assert!(context.get(field).is_none(), "{field}");
    }
    let mut nullable = value;
    nullable["current_request"] = serde_json::Value::Null;
    assert!(serde_json::from_value::<TaskSpaceExplanation>(nullable).unwrap().current_request.is_none());
}
