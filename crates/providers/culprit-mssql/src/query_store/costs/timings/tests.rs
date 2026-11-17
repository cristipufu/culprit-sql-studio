use super::*;
use crate::{
    query_store::{costs::Compilation, tests::compilation_row},
    resources::test_support::replace,
    value::Val,
};

#[test]
fn shared_projection_changes_only_columns_not_native_membership_caps_or_tagging() {
    let columns = columns();
    assert_eq!(columns.matches(SYNAPSE_STAGE_TIMING_SENTINELS).count(), 6);
    for preserved in ["q.last_compile_duration", "q.avg_optimize_duration,q.last_optimize_duration"] {
        assert!(columns.contains(preserved));
    }
    for (name, kind) in [
        ("avg_bind_duration", "float"),
        ("last_bind_duration", "bigint"),
        ("avg_bind_cpu_time", "float"),
        ("last_bind_cpu_time", "bigint"),
        ("avg_optimize_cpu_time", "float"),
        ("last_optimize_cpu_time", "bigint"),
    ] {
        assert!(columns.contains(&format!(
            "CASE WHEN {SYNAPSE_STAGE_TIMING_SENTINELS} THEN CAST(NULL AS {kind}) ELSE q.{name} END AS {name}"
        )));
    }
    let selected = super::super::compilation_sql();
    let retained = super::super::compilation_snapshot_sql();
    assert!(selected.contains(&columns) && retained.contains(&columns));
    assert!(selected.contains("q.query_id IN(SELECT") && selected.contains("@ids.nodes('/ids/id')"));
    assert!(retained.contains("TOP (@P1)"));
    for sql in [selected, retained] {
        assert!(sql.contains("/* culprit */") && sql.contains("OPTION(MAXDOP 1)"));
        assert_eq!(sql.matches("FROM sys.query_store_query q").count(), 1);
        assert!(!sql.contains("sys.query_store_runtime_stats") && !sql.contains("CROSS JOIN"));
    }
}

#[test]
fn decoder_keeps_last_bigints_exact_and_never_derives_total_cpu_from_phases() {
    let row = replace(
        &compilation_row(),
        &[
            ("last_compile_duration", Val::Int(9_007_199_254_740_993)),
            ("last_bind_cpu_time", Val::Int(i64::MAX)),
            ("avg_optimize_cpu_time", Val::Float(0.0)),
        ],
    );
    let native = Compilation::decode(&row).unwrap();
    let observation = native.observation();
    assert_eq!(observation.timings.last_duration_us.as_deref(), Some("9007199254740993"));
    assert_eq!(observation.timings.binding.last_cpu_us, Some(i64::MAX.to_string()));
    assert_eq!(observation.timings.optimization.average_cpu_us, Some(0.0));
    assert_eq!(observation.timings.binding.average_duration_us, Some(250.5));
    assert!(observation.is_valid());
    assert_eq!(native.record().unwrap().key, observation.key().unwrap());
    let json = serde_json::to_value(&observation).unwrap();
    assert_eq!(json["timings"]["last_duration_us"], "9007199254740993");
    assert!(json.get("total_compile_cpu_us").is_none());
    assert_eq!(
        native.history(60).total_ms,
        native.history(1).total_ms,
        "stage data does not move compilation into the runtime window"
    );
}

#[test]
fn unavailable_stages_do_not_replace_real_zero_or_discard_independent_supported_durations() {
    let fields = [
        "avg_bind_duration",
        "last_bind_duration",
        "avg_bind_cpu_time",
        "last_bind_cpu_time",
        "avg_optimize_cpu_time",
        "last_optimize_cpu_time",
    ];
    let row = replace(&compilation_row(), &fields.map(|key| (key, Val::Null)));
    let native = Compilation::decode(&row).unwrap().observation();
    assert_eq!(native.timings.binding, MssqlCompilationStage::default());
    assert_eq!(native.timings.optimization.average_cpu_us, None);
    assert_eq!(native.timings.optimization.last_cpu_us, None);
    assert_eq!(native.timings.optimization.average_duration_us, Some(1000.25));
    assert_eq!(native.timings.last_duration_us.as_deref(), Some("4300"));
    for (field, value) in [("avg_bind_duration", Val::Float(0.0)), ("last_bind_cpu_time", Val::Int(0))] {
        let observed = Compilation::decode(&replace(&row, &[(field, value)])).unwrap().observation();
        assert!(observed.is_valid());
        assert!(
            observed.timings.binding.average_duration_us == Some(0.0)
                || observed.timings.binding.last_cpu_us.as_deref() == Some("0")
        );
    }
}

#[test]
fn malformed_or_missing_native_values_fail_closed_without_numeric_coercion() {
    for (field, value) in [
        ("last_compile_duration", Val::Int(-1)),
        ("last_bind_duration", Val::Float(1.0)),
        ("last_bind_cpu_time", Val::Text("9007199254740993".into())),
        ("avg_bind_cpu_time", Val::Float(f64::NAN)),
        ("avg_optimize_cpu_time", Val::Float(f64::INFINITY)),
        ("avg_optimize_duration", Val::Float(-0.5)),
        ("last_optimize_duration", Val::Bool(false)),
    ] {
        assert!(Compilation::decode(&replace(&compilation_row(), &[(field, value)])).is_err(), "{field}");
    }
    let original = compilation_row();
    let fields = original
        .columns()
        .filter(|(name, _)| *name != "last_compile_duration")
        .map(|(name, value)| (name, value.clone()))
        .collect::<Vec<_>>();
    assert!(Compilation::decode(&Rec::of(&fields)).is_err(), "absent declared projection column is not native NULL");
}
