use super::*;
fn point() -> NativeCounterSet {
    let mut point = sample();
    point.ratios[0].kind = CounterRatioKind::LockWaitAverageInputs;
    for row in &mut point.counters {
        row.native_object = "SQLServer:Locks".into();
        row.native_instance = Some("_Total".into());
    }
    let numerator = &mut point.counters[0];
    numerator.nature = CounterNature::AverageNumerator;
    numerator.unit = CounterUnit::Milliseconds;
    numerator.native_type = Some("1073874176".into());
    numerator.value = MeasurementValue::Integer("4500".into());
    numerator.native_name = "Average Wait Time (ms)".into();
    point.counters[1].native_name = "Average Wait Time Base".into();
    point.counters[1].value = MeasurementValue::Integer("3".into());
    point
}
#[test]
fn complete_average_wait_inputs_are_exact_native_metrics_without_a_percentage_mean_or_time_window() {
    let report = explain(&[point()], None, true).unwrap();
    let entry = &report.entries[0];
    assert_eq!(entry.kind, CounterRatioKind::LockWaitAverageInputs);
    assert_eq!(entry.title, t!("view.performance_counters.lock_wait_average.title"));
    assert_eq!(entry.metrics.len(), 2);
    assert_eq!(entry.metrics[0].text.as_deref(), Some("4500"));
    assert_eq!(entry.metrics[1].text.as_deref(), Some("3"));
    assert_eq!(entry.issues, [CounterRatioIssue::IntervalRequired]);
    assert_eq!(entry.scope, MeasurementScope::Server);
    assert!(entry.explanation.contains(&t!("view.counter_ratios.issue.interval_required")));
    assert!(entry.metrics.iter().all(|metric| metric.key != "view.counter_ratios.metric.percent"));
    let json = serde_json::to_value(&report).unwrap();
    for absent in ["severity", "status", "rate", "database", "query", "fix_sql", "observations"] {
        assert!(json.get(absent).is_none());
    }
}
#[test]
fn absent_average_base_is_unknown_in_a_partial_original_and_zero_is_not_absence() {
    let mut point = point();
    point.counters[0].value = MeasurementValue::Integer("0".into());
    point.counters.pop();
    point.ratios[0].base = None;
    let source = SourceOutcome {
        source: culprit_core::source_id::PERFORMANCE_COUNTERS.into(),
        state: SourceState::Partial,
        reason: None,
        reason_message: None,
    };
    let report = explain(&[point], Some(&source), false).unwrap();
    let entry = &report.entries[0];
    assert_eq!(entry.metrics[0].text.as_deref(), Some("0"));
    assert_eq!(entry.metrics[1].text.as_deref(), Some(t!("view.counter_ratios.unknown").as_str()));
    assert!(entry.issues.contains(&CounterRatioIssue::MissingBase));
    assert!(entry.issues.contains(&CounterRatioIssue::IntervalRequired));
    assert_eq!(report.source_state, Some(SourceState::Partial));
    assert!(report.limitations.contains(&t!("view.counter_ratios.limit.read")));
    assert_eq!(entry.metrics.len(), 2);
}
