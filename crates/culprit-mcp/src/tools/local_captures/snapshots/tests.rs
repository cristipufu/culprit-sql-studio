use super::*;
use culprit_core::{MemoryParticipantExplanation, telemetry::*};

fn original_grant() -> ExecutionGrantObservation {
    let grant: ExecutionGrantObservation = serde_json::from_value(json!({
        "source":"current request memory", "identity":NativeIdentity::new("42:0"),
        "observed_at":"2020-01-01T12:00:00Z", "state":"granted", "request_association":"generation-checked",
        "statement_relationship":"observed-current-statement", "scope":"database",
        "requested_without_offset":"2020-01-01T10:00:00.003", "granted_without_offset":"2020-01-01T10:00:00.006",
        "resource_observation":null, "workload_group_tempdb":null, "queue":ExecutionGrantQueue::default(),"native_properties":{},"available_columns":[],"native_null_columns":[],
        "request":{
            "key":{"session":{"id":"42","generation":"native-login"},"request":{"id":"0","generation":"native-request"}},
            "blocking_status":"known","idle":false,"scope":"request-database","state":"running","database":"db",
            "session_counters":{},"request_counters":{},"statement_identity":NativeIdentity::new("native-statement"),
            "statement":DefinitionPayload::Unavailable{reason:PayloadUnavailable::NotCollected}
        }
    })).unwrap();
    assert!(grant.is_valid());
    grant
}
fn explanation(grant: &ExecutionGrantObservation) -> MemoryParticipantExplanation {
    MemoryParticipantExplanation {
        observation_key: grant.key().unwrap(),
        observation_identity: grant.identity.clone(),
        qualified_request: grant.request.as_ref().map(|request| request.key.clone()),
        mechanism: None,
        title: String::new(),
        explanation: String::new(),
        investigation: String::new(),
        verify: String::new(),
        grant_admission: None,
    }
}

#[test]
fn an_unqualified_request_keeps_its_original_raw_fact_but_cannot_supply_positive_context() {
    let grant = original_grant();
    let original = grant.clone();
    let mut entry = explanation(&grant);
    assert!(std::ptr::eq(qualified_grant_request(&entry, &grant).unwrap().unwrap(), grant.request.as_ref().unwrap()));
    entry.qualified_request = None;
    assert!(qualified_grant_request(&entry, &grant).unwrap().is_none());
    assert_eq!(grant, original);
}

#[test]
fn a_qualified_reference_cannot_select_another_session_request_transaction_or_native_grant() {
    let grant = original_grant();
    for field in ["login", "request-id", "request-generation", "transaction", "grant"] {
        let mut entry = explanation(&grant);
        match field {
            "login" => entry.qualified_request.as_mut().unwrap().session.generation = "foreign-login".into(),
            "request-id" => entry.qualified_request.as_mut().unwrap().request.as_mut().unwrap().id = "1".into(),
            "request-generation" => {
                entry.qualified_request.as_mut().unwrap().request.as_mut().unwrap().generation =
                    "foreign-request".into()
            }
            "transaction" => entry.qualified_request.as_mut().unwrap().transaction = Some("foreign-transaction".into()),
            _ => entry.observation_identity.id = "43:0".into(),
        }
        assert_eq!(
            qualified_grant_request(&entry, &grant).unwrap_err().detail,
            Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::Integrity)),
            "{field}"
        );
    }
    let entry = explanation(&grant);
    let mut missing = grant;
    missing.request = None;
    assert!(qualified_grant_request(&entry, &missing).is_err());
}

#[test]
fn compact_shared_admission_resource_keeps_signed_exact_zero_and_unknown_values_without_native_payloads() {
    let mut grant = original_grant();
    assert_eq!(native_grant_resource(&grant), Value::Null);
    grant.resource = Some(NativeIdentity::new("private-resource-id"));
    grant.resource_observation = Some(ExecutionGrantResourceObservation {
        available_kibibytes: Some("-64".into()),
        granted_kibibytes: Some("9007199254740993".into()),
        grantee_count: Some("0".into()),
        waiter_count: None,
    });
    assert!(grant.is_valid());
    let original = grant.clone();
    let value = native_grant_resource(&grant);
    assert_eq!(
        value,
        json!({
            "qualification":culprit_core::t!("view.request_memory.resource.limit"),
            "available_kibibytes":"-64", "granted_kibibytes":"9007199254740993", "grantee_count":"0", "waiter_count":null,
        })
    );
    assert!(!value.to_string().contains("private-resource-id"));
    assert_eq!(grant, original);
    grant.resource_observation.as_mut().unwrap().available_kibibytes = None;
    assert_eq!(native_grant_resource(&grant)["available_kibibytes"], Value::Null);
}

#[test]
fn compact_group_tempdb_keeps_shared_current_peak_reset_and_abort_history_without_private_originals() {
    let mut grant = original_grant();
    assert_eq!(native_workload_group_tempdb(&grant), Value::Null);
    grant.identity.qualifiers.insert("private-native-receipt".into(), "private-source-proof-canary".into());
    grant.resource = Some(
        NativeIdentity::new("2")
            .qualified("kind", "sql-server-resource-governor")
            .qualified("private-native-pool", "private-instance-canary"),
    );
    grant.workload_group = Some(NativeIdentity::new("7").qualified("kind", "sql-server-resource-governor-group"));
    grant.workload_group_tempdb = Some(MssqlWorkloadGroupTempdbObservation {
        group_id: "7".into(),
        pool_id: "2".into(),
        statistics_started_without_offset: Some("2020-01-01T10:00:00.003".into()),
        current_kibibytes: Some("0".into()),
        peak_kibibytes: Some("9007199254740993".into()),
        limit_violations: Some("3".into()),
        stored_limits: Some(MssqlStoredTempdbDataLimits { fixed_mebibytes: None, percent: None }),
    });
    assert!(grant.is_valid());
    let original = grant.clone();
    let value = native_workload_group_tempdb(&grant);
    assert_eq!(value["current_kibibytes"], "0");
    assert_eq!(value["peak_kibibytes"], "9007199254740993");
    assert_eq!(value["limit_violations"], "3");
    assert!(value["stored_limits"].is_object());
    assert_eq!(value["stored_limits"]["fixed_mebibytes"], Value::Null);
    assert_eq!(value["qualification"], culprit_core::t!("view.request_memory.tempdb.limit"));
    for private in [
        "private-source-proof-canary",
        "private-instance-canary",
        "private-native-receipt",
        "sql_handle",
        "plan_handle",
        "statement",
    ] {
        assert!(!value.to_string().contains(private));
    }
    assert_eq!(grant, original);
    let group = grant.workload_group_tempdb.as_mut().unwrap();
    group.current_kibibytes = None;
    group.limit_violations = None;
    group.statistics_started_without_offset = None;
    group.stored_limits = None;
    let unknown = native_workload_group_tempdb(&grant);
    for field in ["current_kibibytes", "limit_violations", "statistics_started_without_offset", "stored_limits"] {
        assert_eq!(unknown[field], Value::Null);
    }
}

#[test]
fn bounded_database_property_disclosure_retains_recorded_check_metadata_without_offset_or_current_health_claims() {
    struct TestDirectory(std::path::PathBuf);
    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let directory =
        TestDirectory(std::env::temp_dir().join(format!("culprit-mcp-checkdb-{}-{nonce}", std::process::id())));
    std::fs::create_dir(&directory.0).unwrap();
    let workspace = culprit_engine::Workspace::open_in(directory.0.join("workspace"));
    let path = directory.0.join("snapshot.json");
    for (native, expected) in [
        (Some("2019-12-28T23:10:00.003"), Some("2019-12-28T23:10:00.003".to_owned())),
        (Some("1900-01-01T00:00:00.000"), Some(culprit_core::t!("view.database_properties.checkdb_not_recorded"))),
        (None, None),
    ] {
        let mut snapshot: culprit_core::Snapshot =
            serde_json::from_str(include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/mssql-demo.json")))
                .unwrap();
        let mut point: DatabasePropertiesObservation = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../fixtures/database-properties.native.json"
        )))
        .unwrap();
        point.last_good_checkdb_without_offset = native.map(str::to_owned);
        snapshot.operations.database_properties = Some(Box::new(DatabasePropertiesRead {
            target: culprit_core::DatabaseIdentity::server(
                culprit_core::Engine::Mssql,
                "sample",
                "fixture.invalid",
                1433,
                None,
            )
            .into(),
            context: ObservationReadContext::Collector {
                observed_from: "2020-01-01T12:00:00Z".into(),
                observed_until: "2020-01-01T12:00:01Z".into(),
            },
            source: StateSourceCoverage::collected(DATABASE_PROPERTIES_SOURCE.into(), 1),
            observation: Some(Box::new(point)),
            read_complete: true,
        }));
        std::fs::write(&path, serde_json::to_vec(&snapshot).unwrap()).unwrap();
        // File reanalysis invokes the production pure interpretation without a provider or new dependency.
        let report = workspace.analyze_file(&path).unwrap().database_properties.unwrap();
        let value = database_properties(&report);
        assert_eq!(value["fields_available"], 17);
        assert_eq!(value["fields_presented"], ENTRY_LIMIT);
        assert_eq!(value["fields_omitted"], 12);
        assert_eq!(value["fields"][4]["key"], "last_good_checkdb_without_offset");
        assert_eq!(value["fields"][4]["value"], json!(expected));
        assert_eq!(value["observed_at"], "2020-01-01T12:00:00.0000001Z");
        assert!(
            value["limitations"]
                .as_array()
                .unwrap()
                .contains(&json!(culprit_core::t!("core.engines.mssql.integrity_history")))
        );
        assert!(!serde_json::to_string(&value).unwrap().contains("1900-01-01"));
        assert!(value.get("age_seconds").is_none() && value.get("fix_sql").is_none());
    }
}
