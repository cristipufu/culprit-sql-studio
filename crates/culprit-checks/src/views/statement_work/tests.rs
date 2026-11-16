use super::*;
use culprit_core::{ResetMarker, telemetry::*};

fn context() -> NativeReadContext {
    NativeReadContext {
        target: culprit_core::DatabaseIdentity::server(
            culprit_core::Engine::Mysql,
            "db",
            "fixture.invalid",
            3306,
            None,
        )
        .into(),
        partition: SourcePartition::Target,
        observed_from: "2020-01-01T12:00:00Z".into(),
        observed_until: "2020-01-01T12:00:01Z".into(),
        database_writable: None,
    }
}

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
    let report = explain(&context(), &[&observation], None).unwrap();
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
    let observed = explain(&context(), &[&zero], None).unwrap();
    assert!(!observed.entries[0].work[0].metrics.is_empty());
    assert!(!observed.limitations.contains(&t!("view.statement_work.limit.unmeasured")));
    let unknown = row(RuntimeMetricKey::Executions, "0");
    let unknown = explain(&context(), &[&unknown], None).unwrap();
    assert_eq!(unknown.entries.len(), 1);
    assert!(unknown.entries[0].work.is_empty());
    assert!(unknown.limitations.contains(&t!("view.statement_work.limit.unmeasured")));
}

#[test]
fn conflicting_source_state_duplicate_identity_or_non_database_scope_is_rejected() {
    let observation = row(RuntimeMetricKey::TemporaryTables, "1");
    assert!(explain(&context(), &[&observation, &observation], None).is_err());
    for state in [SourceState::Empty, SourceState::Unavailable, SourceState::NotApplicable] {
        let source = SourceOutcome { source: observation.source.clone(), state, reason: None, reason_message: None };
        assert!(explain(&context(), &[&observation], Some(&source)).is_err());
    }
    for scope in [MeasurementScope::Server, MeasurementScope::Pool] {
        let mut changed = observation.clone();
        let OperationalFact::StatementCounters(sample) = &mut changed.fact else { panic!() };
        sample.scope = scope;
        assert!(explain(&context(), &[&changed], None).is_err());
    }
    let source = SourceOutcome {
        source: "another-source".into(),
        state: SourceState::Collected,
        reason: None,
        reason_message: None,
    };
    assert!(explain(&context(), &[&observation], Some(&source)).is_err());
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
        let report = explain(&context(), &[], Some(&source)).unwrap();
        assert_eq!(report.source_state, Some(state));
        assert_eq!(report.source_reason, source.reason);
        assert!(report.entries.is_empty());
    }
}

fn node_context() -> NativeReadContext {
    let mut context = context();
    context.target =
        culprit_core::DatabaseIdentity::server(culprit_core::Engine::Yugabyte, "db", "fixture.invalid", 5433, None)
            .into();
    context.partition = SourcePartition::InstanceLocal {
        member: culprit_core::engines::ysql_node_member("0123456789abcdef0123456789abcdef"),
        epoch: Some("native-process-generation".into()),
    };
    context
}

fn node_row(user: &str, top: &str) -> OperationalObservation {
    let sample = StatementCounterSample {
        identity: NativeQueryIdentity {
            query_id: "-42".into(),
            qualifiers: [
                ("node".into(), "0123456789abcdef0123456789abcdef".into()),
                ("database_oid".into(), "16384".into()),
                ("userid".into(), user.into()),
                ("toplevel".into(), top.into()),
            ]
            .into(),
        },
        scope: MeasurementScope::Node,
        observed_at: "2020-01-01T12:00:00.123456789Z".into(),
        reset: ResetMarker::Unknown,
        totals: vec![StatementCounter {
            key: RuntimeMetricKey::TemporaryBlocksWritten,
            unit: RuntimeMetricUnit::Pages { bytes_per_page: 8192 },
            total: MeasurementValue::Integer("9007199254740993".into()),
        }],
        distributions: vec![],
    };
    OperationalObservation {
        source: "source-owned-native-counters".into(),
        key: sample.key().unwrap(),
        fact: OperationalFact::StatementCounters(Box::new(sample)),
    }
}

#[test]
fn qualified_node_points_keep_each_native_context_and_reset_without_combining_or_timing_them() {
    let rows = [node_row("10", "true"), node_row("11", "true"), node_row("10", "false")];
    let report = explain(&node_context(), &rows.iter().collect::<Vec<_>>(), None).unwrap();
    assert_eq!(report.entries.len(), 3);
    assert!(report.entries.iter().all(|entry| entry.reset == ResetMarker::Unknown));
    for row in &rows {
        let entry = report.entries.iter().find(|entry| entry.observation_key == row.key).unwrap();
        let OperationalFact::StatementCounters(sample) = &row.fact else { panic!() };
        assert_eq!(entry.identity, sample.identity);
        assert!(entry.work[0].metrics.iter().any(|metric| metric.text.as_deref() == Some("9007199254740993")));
    }
    assert!(report.limitations.contains(&t!("view.statement_work.limit.node")));
    assert!(report.limitations.contains(&t!("view.statement_work.limit.reset")));
}

#[test]
fn node_labels_cannot_replace_matching_native_member_generation_or_observation_bounds() {
    let row = node_row("10", "true");
    for changed in ["target", "member", "epoch", "other-member", "other-engine", "time"] {
        let mut context = node_context();
        match changed {
            "target" => context.partition = SourcePartition::Target,
            "member" => {
                context.partition = SourcePartition::InstanceLocal { member: None, epoch: Some("epoch".into()) }
            }
            "epoch" => {
                context.partition = SourcePartition::InstanceLocal {
                    member: culprit_core::engines::ysql_node_member("0123456789abcdef0123456789abcdef"),
                    epoch: None,
                }
            }
            "other-member" => {
                context.partition = SourcePartition::InstanceLocal {
                    member: culprit_core::engines::ysql_node_member("11111111111111111111111111111111"),
                    epoch: Some("epoch".into()),
                }
            }
            "other-engine" => context.target = self::context().target,
            _ => context.observed_until = "2020-01-01T12:00:00.1Z".into(),
        }
        assert!(explain(&context, &[&row], None).is_err(), "{changed}");
    }
}

#[test]
fn partial_node_visibility_keeps_positive_points_and_missing_rows_explicit() {
    let row = node_row("10", "true");
    let source =
        SourceOutcome { source: row.source.clone(), state: SourceState::Partial, reason: None, reason_message: None };
    let report = explain(&node_context(), &[&row], Some(&source)).unwrap();
    assert_eq!(report.source_state, Some(SourceState::Partial));
    assert_eq!(report.entries.len(), 1);
    let missing = explain(&node_context(), &[], Some(&source)).unwrap();
    assert_eq!(missing.source_state, Some(SourceState::Partial));
    assert!(missing.entries.is_empty());
}
