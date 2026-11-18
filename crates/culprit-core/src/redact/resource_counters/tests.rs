use crate::{MeasurementScope, Snapshot, SourceOutcome, SourceState, telemetry::*};
use std::collections::BTreeMap;

fn clerk() -> NativeCounterSet {
    let name = "clerk_private_name_canary";
    NativeCounterSet {
        identity: NativeIdentity::new("clerk_private_type_canary")
            .qualified("native_object", MEMORY_CLERK_NATIVE_OBJECT)
            .qualified("memory_node_id", "0")
            .qualified("name_sha256", digest(name.as_bytes())),
        scope: MeasurementScope::Server,
        counters: vec![NativeCounterSample::gauge(
            "clerk_count",
            2,
            CounterUnit::Count,
            MeasurementScope::Server,
            MEMORY_CLERK_NATIVE_OBJECT,
            "COUNT_BIG(*)",
        )],
        ratios: vec![],
        ratio_gaps: vec![],
        properties: BTreeMap::from([
            ("type".into(), "clerk_private_type_canary".into()),
            ("name".into(), name.into()),
            ("memory_node_id".into(), "0".into()),
            ("aggregation".into(), "clerk-type-name-and-memory-node".into()),
        ]),
    }
}
fn other() -> NativeCounterSet {
    NativeCounterSet {
        identity: NativeIdentity::new("unrelated_private_identity_canary"),
        scope: MeasurementScope::Server,
        counters: vec![NativeCounterSample::gauge(
            "unrelated_private_key_canary",
            9_007_199_254_740_993,
            CounterUnit::Count,
            MeasurementScope::Server,
            "other_private_object_canary",
            "other_private_column_canary",
        )],
        ratios: vec![],
        ratio_gaps: vec![],
        properties: BTreeMap::new(),
    }
}
fn snapshot(points: Vec<NativeCounterSet>) -> Snapshot {
    let mut snapshot: Snapshot = serde_json::from_str(include_str!("../../../../../fixtures/mssql-demo.json")).unwrap();
    snapshot.sources = vec![SourceOutcome {
        source: crate::source_id::MEMORY_CLERKS.into(),
        state: SourceState::Collected,
        reason: None,
        reason_message: None,
    }];
    snapshot.pressure.get_or_insert_with(Default::default).resource_counters = Some(points);
    snapshot
}

#[test]
fn clerk_withholding_keeps_unrelated_exact_counters_and_a_repeatable_family_gap() {
    let original = snapshot(vec![clerk(), other()]);
    assert!(
        original.pressure.as_ref().unwrap().resource_counters.as_ref().unwrap().iter().all(NativeCounterSet::is_valid)
    );
    let shared = crate::redact::redact_snapshot(&original);
    let rows = shared.pressure.as_ref().unwrap().resource_counters.as_ref().unwrap();
    assert_eq!(rows.len(), 1);
    assert!(rows[0].is_valid());
    assert_eq!(rows[0].counters[0].value.integer(), Some(9_007_199_254_740_993));
    assert_eq!(rows[0].counters[0].unit, CounterUnit::Count);
    let text = serde_json::to_string(&shared).unwrap();
    assert!(!text.contains("_canary"));
    let source = shared.sources.iter().find(|source| source.source == crate::source_id::MEMORY_CLERKS).unwrap();
    assert_eq!(source.state, SourceState::Unavailable);
    assert_eq!(source.reason_message, Some(crate::message!("collected.memory_clerks.sharing_withheld")));
    let again = crate::redact::redact_snapshot(&shared);
    let source_again = again.sources.iter().find(|source| source.source == crate::source_id::MEMORY_CLERKS).unwrap();
    assert_eq!(source_again.reason_message, source.reason_message);
    assert_eq!(source_again.state, SourceState::Unavailable);
    assert_eq!(
        serde_json::to_value(&again.pressure.as_ref().unwrap().resource_counters).unwrap(),
        serde_json::to_value(&shared.pressure.as_ref().unwrap().resource_counters).unwrap()
    );
    assert!(serde_json::to_string(&original).unwrap().contains("clerk_private_name_canary"));
}

#[test]
fn malformed_clerk_keys_are_withheld_before_masking_without_poisoning_unrelated_counters() {
    let mut malformed = clerk();
    malformed.counters.push(malformed.counters[0].clone());
    malformed.properties.insert("private_property_canary".into(), "private_value_canary".into());
    assert!(!malformed.is_valid());
    let shared = crate::redact::redact_snapshot(&snapshot(vec![malformed, other()]));
    assert!(!serde_json::to_string(&shared).unwrap().contains("_canary"));
    let rows = shared.pressure.as_ref().unwrap().resource_counters.as_ref().unwrap();
    assert_eq!(rows.len(), 1);
    assert!(rows[0].is_valid());
    assert!(
        shared
            .sources
            .iter()
            .any(|source| source.source == crate::source_id::MEMORY_CLERKS && source.state == SourceState::Unavailable)
    );
    assert!(
        !shared.sources.iter().any(|source| source.source == crate::source_id::PERFORMANCE_COUNTERS
            && source.state == SourceState::Unavailable)
    );
}
