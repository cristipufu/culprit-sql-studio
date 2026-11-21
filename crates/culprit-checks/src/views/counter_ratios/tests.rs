use super::*;
use culprit_core::{MeasurementScope, ResetMarker, Snapshot, telemetry::*};

fn sample() -> NativeCounterSet {
    let counters = [
        ("hits", 9_007_199_254_740_993u64, CounterNature::FractionNumerator, "537003264"),
        ("base", 9_007_199_254_740_994, CounterNature::FractionBase, "1073939712"),
    ]
    .into_iter()
    .map(|(key, value, nature, code)| NativeCounterSample {
        key: key.into(),
        value: MeasurementValue::Integer(value.to_string()),
        unit: CounterUnit::Count,
        nature,
        scope: MeasurementScope::Server,
        reset: ResetMarker::Unknown,
        native_object: "private-cache-owner".into(),
        native_name: key.into(),
        native_instance: None,
        native_type: Some(code.into()),
    })
    .collect();
    NativeCounterSet {
        identity: NativeIdentity::new("native-counter-read"),
        scope: MeasurementScope::Server,
        counters,
        ratios: vec![NativeCounterRatio {
            kind: CounterRatioKind::LocalSsdCacheHit,
            numerator: Some("hits".into()),
            base: Some("base".into()),
            support: CounterRatioSupport::Supported,
            observed_at: Some("2020-01-01T12:00:00.5Z".into()),
        }],
        ratio_gaps: vec![],
        properties: Default::default(),
    }
}

#[test]
fn exact_counts_and_aggregate_ratio_are_text_metrics_without_a_grade_rate_or_current_culprit() {
    let report = explain(&[sample()], None, true).unwrap();
    let entry = &report.entries[0];
    assert_eq!(entry.metrics[0].text.as_deref(), Some("9007199254740993"));
    assert_eq!(entry.metrics[1].text.as_deref(), Some("9007199254740994"));
    assert_eq!(entry.metrics[2].text.as_deref(), Some("99.99%"));
    assert!(entry.metrics.iter().all(|metric| metric.unit == culprit_core::Unit::Text));
    let json = serde_json::to_value(&report).unwrap();
    for absent in ["severity", "status", "rate", "database", "query", "fix_sql", "observations"] {
        assert!(json.get(absent).is_none());
    }
    assert_eq!(report.limitations[1], t!("view.counter_ratios.limit.aggregate"));
}

#[test]
fn missing_and_rejected_inputs_stay_unknown_while_zero_remains_zero() {
    let mut point = sample();
    point.counters[0].value = MeasurementValue::Integer("0".into());
    let report = explain(&[point.clone()], None, true).unwrap();
    assert_eq!(report.entries[0].metrics[0].text.as_deref(), Some("0"));
    assert_eq!(report.entries[0].metrics[2].text.as_deref(), Some("0.00%"));
    point.counters.clear();
    point.ratios[0].numerator = None;
    point.ratios[0].base = None;
    point.properties.insert("actual-read-metadata".into(), "observed".into());
    let report = explain(&[point], None, true).unwrap();
    assert!(report.entries[0].native_object.is_none());
    assert!(report.entries[0].native_instance.is_none());
    assert_eq!(report.entries[0].metrics.len(), 2);
    assert!(
        report.entries[0]
            .metrics
            .iter()
            .all(|metric| metric.text.as_deref() == Some(t!("view.counter_ratios.unknown").as_str()))
    );
}

#[test]
fn partial_and_unavailable_sources_never_become_a_healthy_empty_cache() {
    let source = SourceOutcome {
        source: culprit_core::source_id::PERFORMANCE_COUNTERS.into(),
        state: SourceState::Partial,
        reason: None,
        reason_message: None,
    };
    let report = explain(&[], Some(&source), false).unwrap();
    assert!(!report.read_complete);
    assert!(report.entries.is_empty());
    assert_eq!(report.summary, t!("view.counter_ratios.summary.partial"));
    let wrong = SourceOutcome { source: "other-native-source".into(), ..source };
    assert!(explain(&[sample()], Some(&wrong), true).is_err());
}

#[test]
fn invalid_raw_carrier_is_withheld_with_a_visible_source_problem_by_the_shared_pipeline() {
    let mut snapshot: Snapshot =
        serde_json::from_str(include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/mssql-demo.json")))
            .unwrap();
    let mut point = sample();
    point.ratios[0].base = Some("unobserved-forged-reference".into());
    snapshot.pressure.get_or_insert_with(Default::default).resource_counters = Some(vec![point]);
    let report = crate::analyze(snapshot);
    assert!(report.counter_ratios.is_none());
    assert!(report.snapshot.source_problem(&[culprit_core::source_id::PERFORMANCE_COUNTERS]).is_some());
}

#[test]
fn presentation_limit_does_not_turn_independent_native_populations_into_a_single_total() {
    let samples = (0..20)
        .map(|n| {
            let mut point = sample();
            point.identity.id = format!("native-{n}");
            point
        })
        .collect::<Vec<_>>();
    let report = explain(&samples, None, true).unwrap();
    assert_eq!(report.declared_ratios, 20);
    assert_eq!(report.presented_ratios, MAX_COUNTER_RATIO_ENTRIES as u32);
    assert!(report.entries_limited);
    assert_eq!(report.entries.len(), MAX_COUNTER_RATIO_ENTRIES);
}

#[test]
fn an_observed_uninstanced_descriptor_does_not_borrow_a_rejected_inputs_instance() {
    let mut point = sample();
    let numerator = point.counters.remove(0);
    point.ratio_gaps.push(CounterRatioGap {
        key: numerator.key,
        raw_value: "-1".into(),
        native_object: "different-rejected-object".into(),
        native_name: numerator.native_name,
        native_instance: Some("different-rejected-instance".into()),
        native_type: numerator.native_type.unwrap(),
    });
    assert!(point.is_valid());
    let report = explain(&[point], None, true).unwrap();
    let entry = &report.entries[0];
    assert_eq!(entry.native_object.as_deref(), Some("private-cache-owner"));
    assert_eq!(entry.native_instance, None);
    assert!(entry.issues.contains(&CounterRatioIssue::InvalidNumerator));
    assert_eq!(entry.metrics.len(), 2);
    assert_eq!(entry.metrics[0].text.as_deref(), Some("-1"));
}

mod average_inputs;
