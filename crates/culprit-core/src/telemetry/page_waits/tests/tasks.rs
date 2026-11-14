use super::*;

fn task_observation() -> PageWaitObservation {
    let mut row = observation();
    let resource = ObservedWaitResource {
        kind: WaitResourceKind::MemoryPage,
        native: NativeIdentity::new("5:1:42").qualified("source", "sys.dm_os_waiting_tasks.resource_description"),
    };
    row.task_wait = Some(MssqlPageTaskWait {
        observed_at: row.observed_from.clone(),
        session_id: row.request.key.session.id.parse().unwrap(),
        request_id: row.request.key.request.as_ref().unwrap().id.parse().unwrap(),
        execution_context_id: 3,
        task_address: "00000000ABCDEF01".into(),
        wait_type: "PAGELATCH_EX".into(),
        wait_duration_ms: Some("9007199254740993".into()),
        resource: resource.clone(),
        resource_address: Some("0000000000000000".into()),
        blocking_task_address: Some("00000000DEADBEEF".into()),
        blocking_session_id: Some(-4),
        blocking_execution_context_id: None,
    });
    row.resource = resource;
    row.request.wait = Some(ActivityWait {
        native_type: Some("CXCONSUMER".into()),
        elapsed_ms: Some("1".into()),
        owners: vec![],
        resources: None,
        identified_resources: vec![],
    });
    row
}

#[test]
fn native_task_waits_keep_parallel_resource_duration_and_raw_owner_separate_from_the_coordinator() {
    let row = task_observation();
    assert!(row.is_valid());
    let native = row.observed_wait();
    assert_eq!(native.native_type, Some("PAGELATCH_EX"));
    assert_eq!(native.elapsed_ms, Some("9007199254740993"));
    assert_eq!(native.resource, &row.resource);
    assert_eq!(row.request.wait.as_ref().unwrap().native_type.as_deref(), Some("CXCONSUMER"));
    assert!(row.request.wait.as_ref().unwrap().owners.is_empty());
    let roundtrip: PageWaitObservation = serde_json::from_slice(&serde_json::to_vec(&row).unwrap()).unwrap();
    assert_eq!(roundtrip, row);
    assert_eq!(roundtrip.task_wait.as_ref().unwrap().blocking_session_id, Some(-4));
    assert_eq!(roundtrip.task_wait.as_ref().unwrap().resource_address.as_deref(), Some("0000000000000000"));
}

#[test]
fn task_keys_distinguish_parallel_waiters_without_turning_time_counters_or_owner_fields_into_generations() {
    let row = task_observation();
    let key = row.key().unwrap();
    let mut updated = row.clone();
    let task = updated.task_wait.as_mut().unwrap();
    task.wait_duration_ms = Some("0".into());
    task.blocking_session_id = Some(74);
    task.blocking_task_address = None;
    updated.after_check.as_mut().unwrap().observed_at = "2026-11-13T10:00:00.75Z".into();
    assert!(updated.is_valid());
    assert_eq!(updated.key().unwrap(), key);
    updated.task_wait.as_mut().unwrap().execution_context_id += 1;
    assert_ne!(updated.key().unwrap(), key);
    updated = row.clone();
    updated.task_wait.as_mut().unwrap().task_address = "00000000ABCDEF02".into();
    assert_ne!(updated.key().unwrap(), key);
    updated = row.clone();
    updated.request.key.session.generation = "another-login".into();
    assert_ne!(updated.key().unwrap(), key);
    let mut snapshot = snapshot();
    snapshot.observations = vec![row.clone(), row];
    assert!(!snapshot.is_valid(), "identical task rows cannot be silently collapsed");
}

#[test]
fn task_tuple_clock_resource_and_native_address_must_bind_to_the_original_point() {
    for field in ["session", "request", "clock", "resource", "kind", "address", "source", "duration"] {
        let mut row = task_observation();
        let task = row.task_wait.as_mut().unwrap();
        match field {
            "session" => task.session_id += 1,
            "request" => task.request_id += 1,
            "clock" => task.observed_at = "2026-11-13T10:00:00.4Z".into(),
            "resource" => task.resource.native.id = "5:1:43".into(),
            "kind" => task.resource.kind = WaitResourceKind::PageIo,
            "address" => task.task_address = "0000000000000000".into(),
            "duration" => task.wait_duration_ms = Some("9223372036854775808".into()),
            _ => {
                task.resource.native.qualifiers.clear();
            }
        }
        assert!(!row.is_valid(), "{field}");
    }
}

#[test]
fn missing_changed_or_unverified_task_rechecks_cannot_authorize_current_statement_or_plan_links() {
    for relation in [PageWaitRelation::NoLongerObserved, PageWaitRelation::Different, PageWaitRelation::Unqualified] {
        let mut row = task_observation();
        row.after_check.as_mut().unwrap().wait = relation;
        assert!(!row.is_valid(), "a request that survives does not prove the original task survived");
        row.statement_identity = None;
        row.plan_identity = None;
        assert!(row.is_valid(), "the original task remains useful historical point evidence");
    }
}

#[test]
fn task_evidence_does_not_relax_the_foreign_database_or_shared_tempdb_header_guard() {
    let mut row = task_observation();
    row.request.scope = ActivityScopeBasis::OtherDatabase;
    assert!(!row.is_valid());
    row.target = None;
    row.resolution = PageWaitResolution::OutsideDatabase;
    assert!(row.is_valid(), "unmapped original wait survives without inventing shared target authority");
}
