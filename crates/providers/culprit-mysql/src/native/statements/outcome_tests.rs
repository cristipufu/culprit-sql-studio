use super::*;
use culprit_core::{DatabaseIdentity, MeasurementScope};

fn columns() -> BTreeSet<String> {
    ["SCHEMA_NAME", "DIGEST", "DIGEST_TEXT", "COUNT_STAR", "SUM_TIMER_WAIT", "SUM_ERRORS", "SUM_WARNINGS"]
        .into_iter()
        .map(String::from)
        .collect()
}

fn row(errors: Value, warnings: Value) -> Rec {
    Rec::from_values(vec![
        ("schema_name", Value::from("app")),
        ("digest", Value::from("0123456789abcdef0123456789abcdef")),
        ("observed_at", Value::from("2020-01-01T12:00:00.123456Z")),
        ("COUNT_STAR", Value::UInt(17)),
        ("calls", Value::UInt(17)),
        ("SUM_TIMER_WAIT", Value::UInt(200)),
        ("SUM_ERRORS", errors),
        ("SUM_WARNINGS", warnings),
    ])
}

fn database(name: &str) -> DatabaseIdentity {
    DatabaseIdentity::server(Engine::Mysql, name, "fixture.invalid", 3306, None)
}

#[test]
fn scan_and_collector_reuse_exact_outcome_columns_and_native_count_decoding() {
    let row = row(Value::UInt(9_007_199_254_740_993), Value::UInt(0));
    let layout = Layout { columns: columns(), cpu_enabled: false, digest_enabled: true, overflow_calls: 0 };
    let native = counters(&row, &layout, "app").unwrap();
    let scan = outcome_snapshot(&[&row], &layout.columns, database("app")).unwrap();
    let OperationalFact::StatementCounters(point) = &scan.observations[0].fact else { panic!() };
    assert_eq!(native.outcomes().unwrap(), point.outcomes().unwrap());
    assert_eq!(point.outcomes().unwrap().errors.as_deref(), Some("9007199254740993"));
    assert_eq!(point.outcomes().unwrap().warnings.as_deref(), Some("0"));
    assert_eq!(point.identity, native.identity);
    assert_eq!(point.scope, MeasurementScope::Database);
    assert_eq!(point.member, None);
    assert_eq!(point.reset, ResetMarker::Unknown);
    assert_eq!(scan.context.as_ref().unwrap().observed_from, point.observed_at);
    assert_eq!(scan.context.as_ref().unwrap().partition, SourcePartition::Target);
    assert!(!scan.coverage.complete_scope);
    let query = layout.query(false, 100, 1024).unwrap();
    assert_eq!(query.matches("SUM_ERRORS").count(), 1);
    assert_eq!(query.matches("SUM_WARNINGS").count(), 1);
}

#[test]
fn missing_or_null_outcome_fields_keep_a_statement_point_with_unknown_counts() {
    let row = row(Value::NULL, Value::NULL);
    let missing = columns().into_iter().filter(|name| name != "SUM_ERRORS" && name != "SUM_WARNINGS").collect();
    assert_eq!(outcome_projection(&missing), "NULL AS SUM_ERRORS,NULL AS SUM_WARNINGS");
    for columns in [&missing, &columns()] {
        let snapshot = outcome_snapshot(&[&row], columns, database("app")).unwrap();
        assert_eq!(snapshot.observations.len(), 1);
        let OperationalFact::StatementCounters(point) = &snapshot.observations[0].fact else { panic!() };
        assert!(!point.outcomes().unwrap().is_recorded());
        assert_eq!(point.totals[0].key, RuntimeMetricKey::Executions);
    }
    let empty = outcome_snapshot(&[], &columns(), database("app")).unwrap();
    assert!(empty.context.is_none() && empty.observations.is_empty());
    assert_eq!(empty.coverage.state, culprit_core::SourceState::Partial);
}

#[test]
fn foreign_schema_and_malformed_outcome_counts_fail_instead_of_becoming_zero_or_hash_only_matches() {
    let row = row(Value::UInt(1), Value::UInt(2));
    assert!(outcome_snapshot(&[&row], &columns(), database("another")).is_err());
    for invalid in [Value::Double(1.0), Value::Int(-1), Value::from("18446744073709551616")] {
        let row = self::row(invalid, Value::UInt(0));
        assert!(outcome_snapshot(&[&row], &columns(), database("app")).is_err());
    }
}
