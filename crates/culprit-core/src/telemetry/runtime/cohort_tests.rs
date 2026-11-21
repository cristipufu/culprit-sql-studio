use super::*;

fn key() -> NativeMeasurementKey {
    NativeMeasurementKey {
        native: NativeIdentity::new("native-fragment")
            .qualified("native-plan-slot", "plan-A")
            .qualified("unrecognized-native-context", "retained-verbatim"),
        references: StatementReferences { query: None, plan: None },
        cohort: None,
    }
}

#[test]
fn absent_cohort_does_not_invent_comparison_authority_and_full_native_identity_roundtrips() {
    let mut native = key();
    assert!(native.is_valid());
    let encoded = serde_json::to_value(&native).unwrap();
    assert!(encoded["cohort"].is_null());
    assert_eq!(serde_json::from_value::<NativeMeasurementKey>(encoded).unwrap(), native);
    let original = native.native.clone();
    native.cohort = Some(NativeIdentity::new("source-declared-population").qualified("outcome", "completed"));
    let restored: NativeMeasurementKey = serde_json::from_str(&serde_json::to_string(&native).unwrap()).unwrap();
    assert_eq!(restored, native);
    assert_eq!(restored.native, original, "comparison authority never removes raw native qualifiers");
}

#[test]
fn malformed_cohort_invalidates_both_runtime_and_wait_measurements() {
    let mut native = key();
    native.cohort = Some(NativeIdentity::new(""));
    assert!(!native.is_valid());
    let runtime = RuntimeMeasurement {
        key: native.clone(),
        executions: "1".into(),
        metrics: vec![RuntimeMetric::average(RuntimeMetricKey::Duration, RuntimeMetricUnit::Milliseconds, 1.0)],
        page_server_reads: None,
        first_execution_at: None,
        last_execution_at: None,
    };
    assert!(!runtime.is_valid());
    let waits = WaitMeasurement {
        key: native,
        category: "native-lock".into(),
        unit: RuntimeMetricUnit::Milliseconds,
        total: MeasurementValue::Integer("1".into()),
        average: None,
        minimum: None,
        maximum: None,
        last: None,
        standard_deviation: None,
        category_description: None,
        outcome_description: None,
    };
    assert!(!waits.is_valid());
}
