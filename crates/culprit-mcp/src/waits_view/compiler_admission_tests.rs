use super::*;
use culprit_core::{
    CompilerAdmissionReport, CompilerGatewayExplanation, CompilerWaiterExplanation, SourceState, t, telemetry::*,
};

fn data() -> Report {
    let mut data: Report = serde_json::from_str(include_str!("../../../../fixtures/mssql-demo.report.json")).unwrap();
    let request: ActivityObservation = serde_json::from_value(json!({
        "key":{"session":{"id":"42","generation":"native-actor-generation-canary"},"request":{"id":"0","generation":"native-request-generation-canary"},"transaction":null},
        "idle":false,"scope":"request-database","state":"suspended","database":"app","command":"SELECT","last_request_age_ms":null,
        "open_transactions":0,"elapsed_ms":"1000","transaction_age_ms":null,"session_counters":{},"request_counters":{},"blocking_status":"unknown","waiting_on_lock":null,
        "wait":{"class":"compilation-admission","identified_resources":[],"resources":null,"native_type":"source-issued-compiler-wait","elapsed_ms":"1000","owners":[]},
        "query_hash":null,"statement_identity":{"id":"native-statement-canary","qualifiers":{}},"client":null,
        "statement":{"state":"unavailable","reason":"not-collected"}
    })).unwrap();
    let mut waiter = CompilerWaiterObservation { observed_at: "2026-11-16T10:00:00Z".into(), request };
    waiter.request.statement =
        DefinitionPayload::sql("SELECT private_value FROM compiler_raw_sql_body_canary", culprit_core::Engine::Mssql);
    data.snapshot.operations.compiler_admission = Some(CompilerAdmissionSnapshot {
        source: culprit_core::source_id::COMPILATION_GATEWAYS.into(),
        context: Some(NativeReadContext {
            target: culprit_core::DatabaseIdentity::server(
                culprit_core::Engine::Mssql,
                "app",
                "private-native-owner-canary.invalid",
                1433,
                None,
            )
            .into(),
            partition: SourcePartition::InstanceLocal {
                member: None,
                epoch: Some("private-native-epoch-canary".into()),
            },
            observed_from: waiter.observed_at.clone(),
            observed_until: waiter.observed_at.clone(),
            database_writable: None,
        }),
        observations: vec![OperationalObservation {
            source: culprit_core::source_id::COMPILATION_GATEWAYS.into(),
            key: waiter.key().unwrap(),
            fact: OperationalFact::CompilerWaiter(Box::new(waiter.clone())),
        }],
    });
    data.compiler_admission = Some(CompilerAdmissionReport {
        source_state: Some(SourceState::Partial),
        source_reason: Some("Synthetic partial native gateway read.".into()),
        title: t!("view.compiler_admission.title"),
        summary: t!("view.compiler_admission.summary.waiter"),
        limitations: vec![t!("view.compiler_admission.limit.ownership")],
        waiters: (0..COMPILER_ADMISSION_ENTRIES + 3)
            .map(|index| CompilerWaiterExplanation {
                observation_key: format!("waiter-proof-{index}"),
                activity: waiter.request.key.clone(),
                title: t!("view.compiler_admission.waiter.title", request = "0"),
                explanation: t!("view.compiler_admission.waiter.explanation"),
                investigation: t!("view.compiler_admission.waiter.investigate"),
                verify: t!("view.compiler_admission.waiter.verify"),
            })
            .collect(),
        gateways: (0..COMPILER_ADMISSION_ENTRIES + 2)
            .map(|index| CompilerGatewayExplanation {
                observation_key: format!("gateway-proof-{index}"),
                identity: NativeIdentity::new("native-gateway-canary")
                    .qualified("private-owner", "private-native-owner-canary"),
                title: t!("view.compiler_admission.gateway.title", gate = "Big Gateway", pool = "2"),
                explanation: "Active: 1; configured limit: 4; waiting: 9007199254740993.".into(),
            })
            .collect(),
    });
    data
}

#[test]
fn compiler_admission_summary_bounds_both_lanes_and_never_serializes_raw_sql_or_native_owner_context() {
    let data = data();
    let value = report(&data);
    let admission = &value["compiler_admission"];
    assert_eq!(admission["waiters_retained"], COMPILER_ADMISSION_ENTRIES + 3);
    assert_eq!(admission["gateways_retained"], COMPILER_ADMISSION_ENTRIES + 2);
    assert_eq!(admission["waiters"].as_array().unwrap().len(), COMPILER_ADMISSION_ENTRIES);
    assert_eq!(admission["gateways"].as_array().unwrap().len(), COMPILER_ADMISSION_ENTRIES);
    assert_eq!(admission["source_state"], "partial");
    assert_eq!(admission["source_reason"], "Synthetic partial native gateway read.");
    let body = serde_json::to_string(admission).unwrap();
    assert!(body.contains("9007199254740993"));
    for private in [
        "compiler_raw_sql_body_canary",
        "private-native-owner-canary",
        "private-native-epoch-canary",
        "native-actor-generation-canary",
        "native-gateway-canary",
        "native-statement-canary",
    ] {
        assert!(!body.contains(private), "{private}");
    }
    assert!(value["operations"].get("compiler_admission").is_none());
    assert!(body.len() < 20_000);
}

#[test]
fn empty_partial_and_unavailable_compiler_reads_keep_their_original_coverage_in_compact_output() {
    for state in [SourceState::Empty, SourceState::Partial, SourceState::Unavailable] {
        let mut data = data();
        let original = data.compiler_admission.as_mut().unwrap();
        original.source_state = Some(state);
        original.waiters.clear();
        original.gateways.clear();
        let value = report(&data);
        assert_eq!(value["compiler_admission"]["source_state"], serde_json::to_value(state).unwrap());
        assert_eq!(value["compiler_admission"]["waiters_retained"], 0);
        assert_eq!(value["compiler_admission"]["gateways_retained"], 0);
        assert!(!value["compiler_admission"]["limitations"].as_array().unwrap().is_empty());
    }
}
