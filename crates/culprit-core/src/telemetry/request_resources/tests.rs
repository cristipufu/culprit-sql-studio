use super::*;

pub(crate) fn grant() -> ExecutionGrantObservation {
    ExecutionGrantObservation {
        source: crate::source_id::CURRENT_REQUEST_MEMORY.into(),
        identity: NativeIdentity::new("42:0").qualified("requested_without_offset", "2020-01-01T10:00:00.003"),
        observed_at: "2020-01-01T08:01:00Z".into(),
        state: ExecutionGrantState::Granted,
        request_association: RequestGrantAssociation::GenerationChecked,
        statement_relationship: GrantStatementRelationship::ObservedCurrentStatement,
        request: Some(super::super::activity::tests::request("42")),
        scope: Some(MeasurementScope::Database),
        requested_without_offset: "2020-01-01T10:00:00.003".into(),
        granted_without_offset: Some("2020-01-01T10:00:00.006".into()),
        requested_kibibytes: Some("9007199254740993".into()),
        required_kibibytes: Some("64".into()),
        granted_kibibytes: Some("1024".into()),
        used_kibibytes: Some("0".into()),
        maximum_used_kibibytes: Some("16".into()),
        ideal_kibibytes: None,
        wait_ms: None,
        timeout_seconds: Some("25".into()),
        degree_of_parallelism: Some("1".into()),
        reserved_workers: None,
        used_workers: None,
        maximum_used_workers: None,
        resource: Some(NativeIdentity::new("2").qualified("resource_semaphore_id", "0")),
        workload_group: None,
        native_properties: Default::default(),
        available_columns: vec!["requested_memory_kb".into(), "ideal_memory_kb".into()],
        native_null_columns: vec!["ideal_memory_kb".into()],
    }
}

#[test]
fn current_grants_preserve_exact_native_values_and_keep_lifetime_identity_independent_of_mutable_counters() {
    let mut value = grant();
    assert!(value.is_valid());
    let key = value.key().unwrap();
    assert_eq!(value.requested_kibibytes.as_deref(), Some("9007199254740993"));
    assert_eq!(value.used_kibibytes.as_deref(), Some("0"));
    assert_eq!(value.ideal_kibibytes, None);
    value.used_kibibytes = Some("512".into());
    value.maximum_used_kibibytes = Some("1024".into());
    value.granted_without_offset = None;
    value.granted_kibibytes = None;
    value.state = ExecutionGrantState::Waiting;
    assert!(value.is_valid());
    assert_eq!(value.key().unwrap(), key);
    value.identity = value.identity.qualified("requested_without_offset", "2020-01-01T10:01:00.003");
    assert_ne!(value.key().unwrap(), key);
}

#[test]
fn unknown_request_and_masked_resources_never_become_healthy_zero_or_an_invented_owner() {
    let mut value = grant();
    value.request = None;
    value.scope = None;
    value.request_association = RequestGrantAssociation::ChangedRequest;
    value.statement_relationship = GrantStatementRelationship::Unresolved;
    value.resource = None;
    assert!(value.is_valid());
    value.scope = Some(MeasurementScope::Database);
    assert!(!value.is_valid());
    value.scope = None;
    value.statement_relationship = GrantStatementRelationship::ObservedCurrentStatement;
    assert!(!value.is_valid());
}

#[test]
fn grant_context_validation_rejects_invalid_scope_counts_time_and_column_presence() {
    for invalid in ["-1", "NaN", "18446744073709551616"] {
        let mut value = grant();
        value.used_kibibytes = Some(invalid.into());
        assert!(!value.is_valid());
    }
    let mut value = grant();
    value.native_null_columns.push("unreported_column".into());
    assert!(!value.is_valid());
    let mut value = grant();
    value.granted_without_offset = Some("not a native timestamp".into());
    assert!(!value.is_valid());
    let mut value = grant();
    value.request.as_mut().unwrap().scope = ActivityScopeBasis::OtherDatabase;
    assert!(!value.is_valid());
    let mut value = grant();
    value.request.as_mut().unwrap().key.request = None;
    assert!(!value.is_valid());
}

#[test]
fn request_memory_capture_keeps_empty_source_identity_and_rejects_mixed_or_duplicate_rows() {
    let row = grant();
    let mut captured = RequestMemorySnapshot {
        source: row.source.clone(),
        observed_at: row.observed_at.clone(),
        observations: Vec::new(),
    };
    assert!(captured.is_valid());
    captured.observations.push(row.clone());
    assert!(captured.is_valid());
    captured.observations.push(row);
    assert!(!captured.is_valid());
    captured.observations.pop();
    captured.observations[0].source = "another-native-source".into();
    assert!(!captured.is_valid());
    captured.observations[0].source = captured.source.clone();
    captured.observations[0].observed_at = "2020-01-01T08:01:00.001Z".into();
    assert!(!captured.is_valid(), "a point capture cannot move a row to another sample clock");
    captured.observations.clear();
    captured.observed_at = "2020-01-01T08:01:00".into();
    assert!(!captured.is_valid(), "an empty capture still requires an established UTC clock");
}

#[test]
fn resource_ids_remain_native_and_export_payloads_and_redaction_use_the_existing_activity_contract() {
    let mut value = grant();
    let raw = "SELECT * FROM t WHERE id=42";
    value.request.as_mut().unwrap().statement = DefinitionPayload::Available {
        text: raw.into(),
        sha256: digest(raw.as_bytes()),
        redaction_version: REDACTION_VERSION,
    };
    value.request.as_mut().unwrap().client = Some(ActivityClient {
        application: Some("application-name".into()),
        user: Some("private-user".into()),
        host: None,
    });
    let record = TelemetryRecord::Operational(OperationalObservation {
        source: value.source.clone(),
        key: value.key().unwrap(),
        fact: OperationalFact::ExecutionGrant(Box::new(value.clone())),
    });
    assert!(record.is_valid());
    let fields = record.definition_payloads().collect::<Vec<_>>();
    assert_eq!(fields.len(), 1);
    let json = serde_json::json!({"record": &record});
    assert_eq!(json.pointer(fields[0].pointer).unwrap(), &serde_json::to_value(fields[0].payload).unwrap());
    value.redact(crate::Engine::Mssql);
    assert_eq!(
        value.request.as_ref().unwrap().client.as_ref().unwrap().application.as_deref(),
        Some("application-name")
    );
    assert!(
        matches!(&value.request.as_ref().unwrap().statement,DefinitionPayload::Available{text,..} if !text.contains("42"))
    );
    assert_eq!(value.resource.as_ref().unwrap().qualifiers["resource_semaphore_id"], "0");
}

#[test]
fn sharing_request_memory_removes_client_identity_and_literals_without_changing_the_collected_evidence() {
    let mut snapshot: crate::Snapshot =
        serde_json::from_str(include_str!("../../../../../fixtures/mssql-demo.json")).unwrap();
    let mut value = grant();
    let raw = "SELECT * FROM t WHERE name=N'private-query-canary'";
    let request = value.request.as_mut().unwrap();
    request.statement = DefinitionPayload::Available {
        text: raw.into(),
        sha256: digest(raw.as_bytes()),
        redaction_version: REDACTION_VERSION,
    };
    request.client = Some(ActivityClient {
        application: Some("private-application-canary".into()),
        user: Some("private-user-canary".into()),
        host: Some("private-host-canary".into()),
    });
    snapshot.pressure.get_or_insert_default().request_memory = Some(RequestMemorySnapshot {
        source: value.source.clone(),
        observed_at: value.observed_at.clone(),
        observations: vec![value.clone()],
    });

    let shared = crate::redact::redact_snapshot(&snapshot);
    let serialized = serde_json::to_string(&shared).unwrap();
    assert!(!serialized.contains("-canary"));
    let shared_grant = &shared.pressure.as_ref().unwrap().request_memory.as_ref().unwrap().observations[0];
    assert!(shared_grant.request.as_ref().unwrap().client.is_none());
    assert_eq!(shared_grant.identity, value.identity);
    assert_eq!(shared_grant.scope, value.scope);
    assert_eq!(shared_grant.requested_kibibytes, value.requested_kibibytes);
    assert_eq!(snapshot.pressure.as_ref().unwrap().request_memory.as_ref().unwrap().observations[0], value);
    assert_eq!(
        serde_json::to_value(&shared).unwrap(),
        serde_json::to_value(crate::redact::redact_snapshot(&shared)).unwrap()
    );
}
