use super::*;
use culprit_core::{ResetMarker, telemetry::*};

fn row(key: RuntimeMetricKey, value: &str) -> OperationalObservation {
    let sample = StatementCounterSample {
        identity: NativeQueryIdentity { query_id: "native-query".into(), qualifiers: Default::default() },
        scope: MeasurementScope::Database,
        observed_at: "2020-01-01T12:00:00.123456789Z".into(),
        reset: ResetMarker::Unknown,
        totals: vec![StatementCounter {
            key,
            unit: RuntimeMetricUnit::Count,
            total: MeasurementValue::Integer(value.into()),
        }],
        distributions: vec![],
    };
    OperationalObservation {
        source: "arbitrary-native-source".into(),
        key: sample.key().unwrap(),
        fact: OperationalFact::StatementCounters(Box::new(sample)),
    }
}

#[test]
fn statement_points_preserve_exact_identity_time_and_reset_and_reuse_native_temp_explanations() {
    let observation = row(RuntimeMetricKey::TemporaryDiskTables, "9007199254740993");
    let report = explain(&[&observation], None).unwrap();
    let entry = &report.entries[0];
    assert_eq!(entry.observation_key, observation.key);
    assert_eq!(entry.identity.query_id, "native-query");
    assert_eq!(entry.observed_at, "2020-01-01T12:00:00.123456789Z");
    assert_eq!(entry.reset, ResetMarker::Unknown);
    assert_eq!(entry.work.len(), 1);
    assert!(entry.work[0].metrics.iter().any(|metric| metric.text.as_deref() == Some("9007199254740993")));
    assert_eq!(report.source_state, None);
    assert!(report.limitations.contains(&t!("view.statement_work.limit.source_unknown")));
    let json = serde_json::to_value(&report).unwrap();
    assert!(json.get("snapshot").is_none() && json.get("workload").is_none());
    assert!(json["entries"][0].get("total_time_ms").is_none());
}

#[test]
fn recorded_zero_and_unmeasured_fields_remain_distinct_without_silently_hiding_a_statement() {
    let zero = row(RuntimeMetricKey::TemporaryTables, "0");
    let observed = explain(&[&zero], None).unwrap();
    assert!(!observed.entries[0].work[0].metrics.is_empty());
    assert!(!observed.limitations.contains(&t!("view.statement_work.limit.unmeasured")));
    let unknown = row(RuntimeMetricKey::Executions, "0");
    let unknown = explain(&[&unknown], None).unwrap();
    assert_eq!(unknown.entries.len(), 1);
    assert!(unknown.entries[0].work.is_empty());
    assert!(unknown.limitations.contains(&t!("view.statement_work.limit.unmeasured")));
}

#[test]
fn conflicting_source_state_duplicate_identity_or_non_database_scope_is_rejected() {
    let observation = row(RuntimeMetricKey::TemporaryTables, "1");
    assert!(explain(&[&observation, &observation], None).is_err());
    for state in [SourceState::Empty, SourceState::Unavailable, SourceState::NotApplicable] {
        let source = SourceOutcome { source: observation.source.clone(), state, reason: None, reason_message: None };
        assert!(explain(&[&observation], Some(&source)).is_err());
    }
    for scope in [MeasurementScope::Server, MeasurementScope::Pool] {
        let mut changed = observation.clone();
        let OperationalFact::StatementCounters(sample) = &mut changed.fact else { panic!() };
        sample.scope = scope;
        assert!(explain(&[&changed], None).is_err());
    }
    let source = SourceOutcome {
        source: "another-source".into(),
        state: SourceState::Collected,
        reason: None,
        reason_message: None,
    };
    assert!(explain(&[&observation], Some(&source)).is_err());
}

#[test]
fn unavailable_empty_and_partial_native_reads_stay_explicit_without_manufacturing_work() {
    for state in [SourceState::Empty, SourceState::Unavailable, SourceState::NotApplicable, SourceState::Partial] {
        let source = SourceOutcome {
            source: "arbitrary-native-source".into(),
            state,
            reason: Some("native source qualification".into()),
            reason_message: None,
        };
        let report = explain(&[], Some(&source)).unwrap();
        assert_eq!(report.source_state, Some(state));
        assert_eq!(report.source_reason, source.reason);
        assert!(report.entries.is_empty());
    }
}
