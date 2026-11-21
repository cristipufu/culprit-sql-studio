use super::*;
use crate::value::Val;

fn row() -> Rec {
    Rec::of(&[
        ("row_id", Val::Int(19)),
        ("plan_id", Val::Int(4)),
        ("replica_group_id", Val::Int(7)),
        ("execution_type", Val::Int(3)),
        ("wait_category", Val::Int(250)),
        ("wait_category_desc", Val::Text("Future wait category".into())),
        ("execution_type_desc", Val::Text("Aborted".into())),
        ("total_query_wait_time_ms", Val::Int(9007199254740993)),
        ("avg_query_wait_time_ms", Val::Float(25.0)),
        ("min_query_wait_time_ms", Val::Int(10)),
        ("max_query_wait_time_ms", Val::Int(9007199254740993)),
        ("last_query_wait_time_ms", Val::Int(9007199254740993)),
        ("stdev_query_wait_time_ms", Val::Float(0.75)),
    ])
}
fn changed(name: &str, value: Val) -> Rec {
    let row = row();
    let fields = row
        .columns()
        .map(|(key, original)| (key, if key == name { value.clone() } else { original.clone() }))
        .collect::<Vec<_>>();
    Rec::of(&fields)
}
fn decode(row: &Rec) -> WaitMeasurement {
    let TelemetryRecord::QueryWaits(wait) =
        record(row, TelemetryStream::QueryWaits, StatementReferences::default()).unwrap()
    else {
        panic!("native wait fragment")
    };
    assert!(wait.is_valid());
    wait
}

#[test]
fn original_wait_fields_preserve_bigint_precision_native_labels_and_independent_population() {
    let wait = decode(&row());
    assert_eq!(wait.last, Some(MeasurementValue::Integer("9007199254740993".into())));
    assert_eq!(wait.standard_deviation, Some(0.75));
    assert_eq!(wait.category_description.as_deref(), Some("Future wait category"));
    assert_eq!(wait.outcome_description.as_deref(), Some("Aborted"));
    assert_eq!(wait.category, "250");
    assert_eq!(wait.key.native.id, "19");
    assert_eq!(wait.key.native.qualifiers["execution_type"], "3");
    assert_eq!(wait.key.native.qualifiers["replica_group_id"], "7");
    assert_eq!(wait.key.cohort.as_ref().unwrap().qualifiers["execution_type"], "3");
    assert!(!wait.key.cohort.unwrap().qualifiers.contains_key("plan_id"));
}

#[test]
fn explicit_native_nulls_remain_unknown_and_missing_or_malformed_columns_fail_closed() {
    for name in ["last_query_wait_time_ms", "stdev_query_wait_time_ms", "wait_category_desc", "execution_type_desc"] {
        let null = decode(&changed(name, Val::Null));
        match name {
            "last_query_wait_time_ms" => assert!(null.last.is_none()),
            "stdev_query_wait_time_ms" => assert!(null.standard_deviation.is_none()),
            "wait_category_desc" => assert!(null.category_description.is_none()),
            _ => assert!(null.outcome_description.is_none()),
        }
        let native = row();
        let fields = native
            .columns()
            .filter(|(key, _)| *key != name)
            .map(|(key, value)| (key, value.clone()))
            .collect::<Vec<_>>();
        assert!(
            record(&Rec::of(&fields), TelemetryStream::QueryWaits, StatementReferences::default()).is_err(),
            "{name}"
        );
    }
    for (name, value) in [
        ("last_query_wait_time_ms", Val::Int(-1)),
        ("last_query_wait_time_ms", Val::Float(1.25)),
        ("stdev_query_wait_time_ms", Val::Float(-1.0)),
        ("stdev_query_wait_time_ms", Val::Float(f64::NAN)),
        ("stdev_query_wait_time_ms", Val::Float(f64::INFINITY)),
        ("wait_category_desc", Val::Text("x".repeat(129))),
        ("execution_type_desc", Val::Int(3)),
    ] {
        assert!(
            record(&changed(name, value), TelemetryStream::QueryWaits, StatementReferences::default()).is_err(),
            "{name}"
        );
    }
}

#[test]
fn native_wait_query_projects_original_fragment_fields_without_grouping_or_extra_statement() {
    let layout = super::super::layout::NativeLayout::decode(&super::super::window_sql::tests::layout_row()).unwrap();
    let sql = super::super::catalog_sql::waits(None, &layout);
    for field in ["wait_category_desc", "execution_type_desc", "last_query_wait_time_ms", "stdev_query_wait_time_ms"] {
        assert!(sql.contains(field), "{field}");
    }
    assert!(sql.contains("SELECT TOP (@P1)"));
    assert!(sql.contains("r.runtime_stats_interval_id=@P2"));
    assert!(sql.contains("r.wait_stats_id OPTION(MAXDOP 1)"));
    assert!(!sql.contains("GROUP BY"));
    assert!(!sql.contains("SUM("));
}
