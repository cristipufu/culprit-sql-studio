use super::*;
fn detailed_sample() -> NativeCounterSet {
    let profile = Engine::Mssql.spec().performance_counters().unwrap();
    let mut value = sample();
    value.ratios.clear();
    value.counters = profile
        .detailed_fields
        .iter()
        .filter(|field| field.family == PerformanceCounterFamily::AccessMethods)
        .map(|field| NativeCounterSample {
            key: field.id.into(),
            value: MeasurementValue::Integer("9007199254740993".into()),
            unit: CounterUnit::Count,
            nature: CounterNature::Cumulative,
            scope: MeasurementScope::Server,
            reset: ResetMarker::Unknown,
            native_object: "SQLServer:Access Methods".into(),
            native_name: field.native_name.into(),
            native_instance: None,
            native_type: Some("272696576".into()),
        })
        .collect();
    value.properties.insert(PERFORMANCE_COUNTER_PROFILE_PROPERTY.into(), MSSQL_COMMON_COUNTER_PROFILE.into());
    value.properties.insert(PERFORMANCE_COUNTER_ROWS_PROPERTY.into(), "6".into());
    value
}

fn missing_access_fields(report: &PerformanceCounterReport) -> usize {
    let profile = Engine::Mssql.spec().performance_counters().unwrap();
    report
        .missing_fields
        .iter()
        .filter(|missing| {
            profile
                .detailed_fields
                .iter()
                .any(|field| field.family == PerformanceCounterFamily::AccessMethods && field.label_key == missing.key)
        })
        .count()
}
#[test]
fn detailed_access_explanation_preserves_operation_totals_and_shared_scope_without_query_or_harm_advice() {
    let report = explain_sample(&detailed_sample()).unwrap();
    assert!(report.database.is_none() && report.native_database_id.is_none());
    assert_eq!(report.entries.len(), 6);
    for entry in &report.entries {
        assert_eq!(entry.family, PerformanceCounterFamily::AccessMethods);
        assert_eq!(entry.family_label, t!("view.performance_counters.family.access_methods"));
        assert_eq!(entry.explanation, t!("view.performance_counters.explain.access_methods"));
        assert_eq!(entry.raw_value, "9007199254740993");
        assert_eq!(
            (entry.scope, entry.nature, entry.unit),
            (MeasurementScope::Server, CounterNature::Cumulative, CounterUnit::Count)
        );
        assert_eq!(entry.reset, ResetMarker::Unknown);
        assert_eq!(entry.availability, PerformanceCounterAvailability::Observed);
    }
    assert!(report.ratios.is_none());
    assert!(report.limitations.contains(&t!("view.performance_counters.limit.lifetime")));
    assert!(report.limitations.contains(&t!("view.performance_counters.limit.cause")));
    assert_eq!(missing_access_fields(&report), 0);
}
#[test]
fn detailed_missing_zero_unknown_native_type_and_wrong_scope_are_independent() {
    let mut sample = detailed_sample();
    sample.counters.truncate(2);
    sample.counters[0].value = MeasurementValue::Integer("0".into());
    sample.counters[1].native_type = Some("42".into());
    sample.counters[1].nature = CounterNature::Unknown;
    sample.counters[1].unit = CounterUnit::Unknown;
    sample.properties.insert(PERFORMANCE_COUNTER_ROWS_PROPERTY.into(), "2".into());
    let report = explain_sample(&sample).unwrap();
    assert_eq!(report.entries[0].raw_value, "0");
    assert_eq!(report.entries[0].availability, PerformanceCounterAvailability::Observed);
    assert_eq!(report.entries[1].availability, PerformanceCounterAvailability::UnknownNativeType);
    assert_eq!(report.entries[1].raw_value, "9007199254740993");
    assert_eq!(missing_access_fields(&report), 4);
    sample.scope = MeasurementScope::Database;
    sample.counters.iter_mut().for_each(|counter| counter.scope = MeasurementScope::Database);
    assert!(explain_sample(&sample).is_err());
}
