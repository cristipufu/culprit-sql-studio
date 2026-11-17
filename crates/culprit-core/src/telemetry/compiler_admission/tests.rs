use super::*;

pub(in crate::telemetry) fn gateway(pool: &str, waiting: u64) -> OperationalObservation {
    let counters = NativeCounterSet {
        ratios: Vec::new(),
        ratio_gaps: Vec::new(),
        identity: NativeIdentity::new("Big Gateway")
            .qualified("native_object", GATEWAY_OBJECT)
            .qualified("resource_pool_id", pool),
        scope: MeasurementScope::Server,
        counters: [
            ("maximum-compilers", "max_count", 1),
            ("active-compilers", "active_count", 1),
            ("waiting-compilers", "waiter_count", waiting),
        ]
        .into_iter()
        .map(|(key, name, value)| {
            NativeCounterSample::gauge(key, value, CounterUnit::Count, MeasurementScope::Server, GATEWAY_OBJECT, name)
        })
        .collect(),
        properties: [
            ("threshold".into(), "-1".into()),
            ("threshold_factor".into(), "8".into()),
            ("is_active".into(), "0".into()),
        ]
        .into(),
    };
    OperationalObservation::from_counters(crate::source_id::COMPILATION_GATEWAYS, counters).unwrap()
}

#[test]
fn native_policy_counts_and_unknown_scan_context_round_trip_without_claiming_memory_or_owner() {
    let row = gateway("2", 9_007_199_254_740_993);
    let facts = CompilerGatewayFacts::from_observation(&row).unwrap();
    assert_eq!(facts.waiting_compilers, 9_007_199_254_740_993);
    assert!(!facts.required);
    assert_eq!(facts.threshold_bytes, None);
    let snapshot = CompilerAdmissionSnapshot { source: row.source.clone(), context: None, observations: vec![row] };
    assert!(snapshot.is_valid());
    assert_eq!(
        serde_json::from_slice::<CompilerAdmissionSnapshot>(&serde_json::to_vec(&snapshot).unwrap()).unwrap(),
        snapshot
    );
    assert!(snapshot.context.is_none(), "a captured scan timestamp cannot manufacture a native read context");
}

#[test]
fn only_the_exact_native_gateway_schema_can_authorize_its_meaning() {
    for defect in ["source", "object", "unit", "nature", "threshold", "missing", "scope", "pool", "duplicate"] {
        let mut row = gateway("2", 1);
        if defect == "source" {
            row.source = "another-native-counter-source".into();
        }
        let OperationalFact::CounterSet(counters) = &mut row.fact else { panic!() };
        match defect {
            "object" => {
                counters.identity.qualifiers.insert("native_object".into(), "another-native-object".into());
            }
            "unit" => counters.counters[0].unit = CounterUnit::Bytes,
            "nature" => counters.counters[0].nature = CounterNature::Cumulative,
            "threshold" => {
                counters.properties.insert("threshold".into(), "-2".into());
            }
            "missing" => {
                counters.counters.pop();
            }
            "scope" => counters.scope = MeasurementScope::Database,
            "pool" => {
                counters.identity.qualifiers.insert("resource_pool_id".into(), "unknown".into());
            }
            "duplicate" => counters.counters.push(counters.counters[0].clone()),
            _ => {}
        }
        row.key = counters.key().unwrap();
        assert!(CompilerGatewayFacts::from_observation(&row).is_err(), "{defect}");
    }
}

#[test]
fn snapshots_reject_duplicate_or_foreign_rows_without_merging_resource_pools() {
    let row = gateway("2", 1);
    let mut snapshot = CompilerAdmissionSnapshot {
        source: row.source.clone(),
        context: None,
        observations: vec![row.clone(), gateway("3", 1)],
    };
    assert!(snapshot.is_valid());
    snapshot.observations.push(row);
    assert!(!snapshot.is_valid());
    snapshot.observations.pop();
    snapshot.source = "another-source".into();
    assert!(!snapshot.is_valid());
}
