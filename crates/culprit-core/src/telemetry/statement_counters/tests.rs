use super::*;
use crate::{DatabaseIdentity, Engine, MeasurementScope, ResetMarker, SourceState};

fn sample(errors: Option<&str>, warnings: Option<&str>) -> StatementCounterSample {
    let mut totals = vec![StatementCounter {
        key: RuntimeMetricKey::Executions,
        unit: RuntimeMetricUnit::Count,
        total: MeasurementValue::Integer("17".into()),
    }];
    for (key, value) in [(RuntimeMetricKey::Errors, errors), (RuntimeMetricKey::Warnings, warnings)] {
        if let Some(value) = value {
            totals.push(StatementCounter {
                key,
                unit: RuntimeMetricUnit::Count,
                total: MeasurementValue::Integer(value.into()),
            });
        }
    }
    StatementCounterSample {
        member: None,
        identity: NativeQueryIdentity {
            query_id: "same-digest".into(),
            qualifiers: [("schema".into(), "private-schema-canary".into())].into(),
        },
        scope: MeasurementScope::Database,
        observed_at: "2020-01-01T12:00:00.123456Z".into(),
        reset: ResetMarker::Unknown,
        totals,
        distributions: vec![],
    }
}

fn snapshot(sample: StatementCounterSample) -> StatementCounterSnapshot {
    StatementCounterSnapshot {
        source: "native-counters".into(),
        context: Some(Box::new(NativeReadContext {
            target: DatabaseIdentity::server(
                Engine::Mysql,
                "private-schema-canary",
                "private-route-canary.invalid",
                3306,
                None,
            )
            .into(),
            partition: SourcePartition::Target,
            observed_from: "2020-01-01T12:00:00Z".into(),
            observed_until: "2020-01-01T12:00:01Z".into(),
            database_writable: None,
        })),
        coverage: StateSourceCoverage::observed("native-counters".into(), 1, false),
        observations: vec![OperationalObservation {
            source: "native-counters".into(),
            key: sample.key().unwrap(),
            fact: OperationalFact::StatementCounters(Box::new(sample)),
        }],
    }
}

#[test]
fn source_declared_error_and_warning_totals_preserve_large_zero_and_missing_values() {
    let point = sample(Some("9007199254740993"), Some("0"));
    assert!(point.is_valid());
    assert_eq!(
        point.outcomes().unwrap(),
        crate::StatementOutcomeCounts { errors: Some("9007199254740993".into()), warnings: Some("0".into()) }
    );
    assert!(point.outcomes().unwrap().is_recorded());
    let missing = sample(None, Some(&u64::MAX.to_string()));
    assert_eq!(missing.outcomes().unwrap().errors, None);
    assert_eq!(missing.outcomes().unwrap().warnings.as_deref(), Some("18446744073709551615"));
    assert!(!sample(None, None).outcomes().unwrap().is_recorded());
}

#[test]
fn native_names_do_not_gain_error_meaning_and_invalid_units_or_real_counts_are_refused() {
    let mut opaque = sample(None, None);
    opaque.totals.push(StatementCounter {
        key: RuntimeMetricKey::Native("mysql.errors".into()),
        unit: RuntimeMetricUnit::Count,
        total: MeasurementValue::Integer("999".into()),
    });
    assert!(!opaque.outcomes().unwrap().is_recorded());
    for (unit, value) in [
        (RuntimeMetricUnit::Bytes, MeasurementValue::Integer("1".into())),
        (RuntimeMetricUnit::Count, MeasurementValue::Real(1.0)),
        (RuntimeMetricUnit::Count, MeasurementValue::Integer("18446744073709551616".into())),
    ] {
        let mut invalid = sample(Some("1"), None);
        invalid.totals[1].unit = unit;
        invalid.totals[1].total = value;
        assert!(!invalid.is_valid() && invalid.outcomes().is_err());
    }
}

#[test]
fn schema_and_full_identity_remain_separate_while_counts_never_gain_a_reset_epoch() {
    let left = sample(Some("1"), None);
    let mut right = left.clone();
    right.identity.qualifiers.insert("schema".into(), "another-schema".into());
    assert_ne!(left.key().unwrap(), right.key().unwrap());
    assert_eq!(left.reset, ResetMarker::Unknown);
    assert_eq!(left.outcomes().unwrap(), right.outcomes().unwrap());
}

#[test]
fn absent_native_context_cannot_certify_a_healthy_empty_source() {
    let mut input = snapshot(sample(Some("0"), None));
    input.context = None;
    assert!(!input.is_valid());
    input.observations.clear();
    input.coverage.records = 0;
    assert!(input.is_valid());
    for state in [SourceState::Collected, SourceState::Empty] {
        input.coverage.state = state;
        input.coverage.complete_scope = true;
        assert!(!input.is_valid());
    }
}

#[test]
fn missing_read_rows_preserve_original_counts_and_each_source_clock_and_namespace_is_checked() {
    let mut input = snapshot(sample(Some("7"), None));
    input.observations.clear();
    assert!(input.is_valid_read(false) && !input.is_valid());
    assert_eq!(input.coverage.records, 1);
    let mut input = snapshot(sample(Some("7"), None));
    input.observations[0].source = "another-source".into();
    assert!(!input.is_valid());
    input.observations[0].source = input.source.clone();
    input.context.as_mut().unwrap().observed_until = "2020-01-01T12:00:00.1Z".into();
    assert!(!input.is_valid());
}

#[test]
fn typed_node_membership_survives_opaque_sharing_without_reparsing_private_native_labels() {
    let mut point = sample(Some("9007199254740993"), Some("0"));
    let member = crate::engines::ysql_node_member("0123456789abcdef0123456789abcdef").unwrap();
    point.scope = MeasurementScope::Node;
    point.member = Some(member.clone());
    point.identity.qualifiers.insert("node".into(), member.native.id.clone());
    let mut input = snapshot(point);
    let context = input.context.as_mut().unwrap();
    // Existing sharing policy preserves logical database labels; opaque native qualifiers remain private.
    context.target =
        DatabaseIdentity::server(Engine::Yugabyte, "public-database", "private-route-canary.invalid", 5433, None)
            .into();
    context.partition =
        SourcePartition::InstanceLocal { member: Some(member), epoch: Some("private-process-canary".into()) };
    assert!(input.is_valid());
    let mut data: Box<crate::Snapshot> =
        serde_json::from_str(include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/mysql-demo.json")))
            .unwrap();
    data.operations.statement_work = Some(Box::new(input));
    let shared = crate::redact::redact_snapshot(&data);
    let retained = shared.operations.statement_work.as_ref().unwrap();
    assert!(retained.is_valid());
    let OperationalFact::StatementCounters(point) = &retained.observations[0].fact else { panic!() };
    assert_eq!(retained.context.as_ref().unwrap().target.database().unwrap().database, "public-database");
    assert!(point.identity.qualifiers["schema"].starts_with("redacted-"));
    assert_eq!(point.outcomes().unwrap().errors.as_deref(), Some("9007199254740993"));
    assert_eq!(point.reset, ResetMarker::Unknown);
    let json = serde_json::to_string(retained).unwrap();
    for private in
        ["private-schema-canary", "private-route-canary", "private-process-canary", "0123456789abcdef0123456789abcdef"]
    {
        assert!(!json.contains(private));
    }
    assert_eq!(
        serde_json::to_value(&shared).unwrap(),
        serde_json::to_value(crate::redact::redact_snapshot(&shared)).unwrap()
    );
}

#[test]
fn export_admission_requires_the_original_typed_member_and_native_point_bounds() {
    let mut point = sample(Some("1"), Some("0"));
    point.scope = MeasurementScope::Node;
    point.member = crate::engines::ysql_node_member("0123456789abcdef0123456789abcdef");
    let mut input = snapshot(point);
    let context = input.context.as_mut().unwrap();
    context.target = DatabaseIdentity::server(Engine::Yugabyte, "app", "fixture.invalid", 5433, None).into();
    context.partition = SourcePartition::InstanceLocal {
        member: crate::engines::ysql_node_member("0123456789abcdef0123456789abcdef"),
        epoch: Some("native-process".into()),
    };
    let source = CollectionDescription {
        target: context.target.clone(),
        source: input.source.clone(),
        partition: context.partition.clone(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::Snapshot(StateSnapshotDescription {
            dataset: SnapshotDataset::StatementCounters,
            observed_from: context.observed_from.clone(),
            observed_until: context.observed_until.clone(),
            records: 1,
            sources: vec![input.coverage.clone()],
        }),
    };
    let capture = CaptureIdentity { observation_id: "native-counter-capture".into(), revision: None };
    let original = TelemetryRecord::Operational(input.observations[0].clone());
    assert!(ExportedRecord::new(&source, &capture, original.clone()).is_ok());
    for change in ["member", "clock"] {
        let mut changed = original.clone();
        let TelemetryRecord::Operational(row) = &mut changed else { panic!() };
        let OperationalFact::StatementCounters(point) = &mut row.fact else { panic!() };
        if change == "member" {
            point.member = crate::engines::ysql_node_member("11111111111111111111111111111111");
        } else {
            point.observed_at = "2020-01-01T11:59:59Z".into();
        }
        assert!(point.is_valid());
        assert!(!point.fits_source_scope(&source));
        assert!(ExportedRecord::new(&source, &capture, changed).is_err(), "{change}");
    }
}
