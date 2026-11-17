use super::*;
use culprit_core::{DatabaseIdentity, Engine, MeasurementScope, SourceState, telemetry::*};

fn gateway(pool: &str, waiting: u64) -> OperationalObservation {
    let object = "sys.dm_exec_query_optimizer_memory_gateways";
    OperationalObservation::from_counters(
        culprit_core::source_id::COMPILATION_GATEWAYS,
        NativeCounterSet {
            ratios: Vec::new(),
            ratio_gaps: Vec::new(),
            identity: NativeIdentity::new("Big Gateway")
                .qualified("native_object", object)
                .qualified("resource_pool_id", pool),
            scope: MeasurementScope::Server,
            counters: [
                ("maximum-compilers", "max_count", 1),
                ("active-compilers", "active_count", 1),
                ("waiting-compilers", "waiter_count", waiting),
            ]
            .into_iter()
            .map(|(key, name, value)| {
                NativeCounterSample::gauge(key, value, CounterUnit::Count, MeasurementScope::Server, object, name)
            })
            .collect(),
            properties: [
                ("threshold".into(), "-1".into()),
                ("threshold_factor".into(), "8".into()),
                ("is_active".into(), "0".into()),
            ]
            .into(),
        },
    )
    .unwrap()
}

fn snapshot() -> CompilerAdmissionSnapshot {
    CompilerAdmissionSnapshot {
        source: culprit_core::source_id::COMPILATION_GATEWAYS.into(),
        context: None,
        observations: vec![gateway("2", 9_007_199_254_740_993), gateway("3", 0)],
    }
}

fn waiter() -> OperationalObservation {
    let request: ActivityObservation = serde_json::from_value(serde_json::json!({
        "key":{"session":{"id":"51","generation":"session-generation"},"request":{"id":"0","generation":"request-generation"},"transaction":null},
        "idle":false,"scope":"request-database","state":"suspended","database":"app","command":"SELECT","last_request_age_ms":null,
        "open_transactions":0,"elapsed_ms":"1000","transaction_age_ms":null,"session_counters":{},"request_counters":{},
        "blocking_status":"unknown","waiting_on_lock":null,
        "wait":{"class":"compilation-admission","identified_resources":[],"resources":null,"native_type":"source-issued-compiler-wait","elapsed_ms":"1000","owners":[]},
        "query_hash":null,"statement_identity":{"id":"native-statement","qualifiers":{}},"client":null,
        "statement":{"state":"unavailable","reason":"not-collected"}
    })).unwrap();
    let waiter = CompilerWaiterObservation { observed_at: "2020-01-01T00:00:00Z".into(), request };
    OperationalObservation {
        source: culprit_core::source_id::COMPILATION_GATEWAYS.into(),
        key: waiter.key().unwrap(),
        fact: OperationalFact::CompilerWaiter(Box::new(waiter)),
    }
}

#[test]
fn raw_shared_counts_remain_exact_independent_gate_context_without_a_memory_total_or_owner_claim() {
    let raw = snapshot();
    let report = explain(&raw, None).unwrap();
    assert_eq!(report.gateways.len(), 2);
    assert!(report.waiters.is_empty());
    assert_eq!(report.gateways[0].observation_key, raw.observations[0].key);
    assert!(report.gateways[0].explanation.contains(&format::digits("9007199254740993".into())));
    assert!(report.limitations.contains(&t!("view.compiler_admission.limit.context")));
    assert!(report.limitations.contains(&t!("view.compiler_admission.limit.ownership")));
    let json = serde_json::to_value(&report).unwrap();
    for absent in ["memory_total", "workload", "fix_sql", "snapshot", "observations"] {
        assert!(json.get(absent).is_none());
    }
}

#[test]
fn an_exact_affected_request_is_retained_with_partial_source_coverage_and_never_assigned_a_pool() {
    let mut raw = snapshot();
    raw.context = Some(NativeReadContext {
        target: DatabaseIdentity::server(Engine::Mssql, "app", "synthetic.invalid", 1433, None).into(),
        partition: SourcePartition::InstanceLocal { member: None, epoch: None },
        observed_from: "2020-01-01T00:00:00Z".into(),
        observed_until: "2020-01-01T00:00:01Z".into(),
        database_writable: None,
    });
    raw.observations.insert(0, waiter());
    let source = SourceOutcome {
        source: raw.source.clone(),
        state: SourceState::Partial,
        reason: Some("synthetic partial native read".into()),
        reason_message: None,
    };
    let report = explain(&raw, Some(&source)).unwrap();
    assert_eq!(report.source_state, Some(SourceState::Partial));
    assert_eq!(report.waiters.len(), 1);
    assert_eq!(report.waiters[0].observation_key, raw.observations[0].key);
    assert_eq!(report.waiters[0].activity.session.generation, "session-generation");
    assert!(report.waiters[0].investigation.contains(&t!("view.compiler_admission.waiter.investigate")));
    let json = serde_json::to_value(&report.waiters[0]).unwrap();
    for absent in ["pool", "gateway", "memory", "query", "cost", "share", "fix_sql"] {
        assert!(json.get(absent).is_none());
    }
}

#[test]
fn invalid_schema_or_source_outcomes_withhold_the_explanation_but_not_the_original_facts() {
    let original = snapshot();
    for state in [SourceState::Empty, SourceState::Unavailable, SourceState::NotApplicable] {
        let source = SourceOutcome { source: original.source.clone(), state, reason: None, reason_message: None };
        assert!(explain(&original, Some(&source)).is_err());
    }
    let mut invalid = original.clone();
    invalid.observations.push(invalid.observations[0].clone());
    assert!(explain(&invalid, None).is_err());
    assert_eq!(original.observations.len(), 2);
    assert!(original.context.is_none());
}
