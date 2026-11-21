use super::*;
use std::collections::BTreeMap;

fn sample() -> NativeCounterSet {
    let mut sample = NativeCounterSet {
        identity: NativeIdentity::new("0").qualified("native_object", MSSQL_SCHEDULER_OBJECT),
        scope: crate::MeasurementScope::Server,
        counters: vec![],
        properties: BTreeMap::new(),
        ratios: vec![],
        ratio_gaps: vec![],
    };
    for field in MSSQL_SCHEDULER_FIELDS {
        match field.kind {
            Identity => (),
            IntegerProperty | BooleanProperty => {
                sample.properties.insert(field.name.into(), "0".into());
            }
            TextProperty => {
                sample.properties.insert(field.name.into(), "VISIBLE ONLINE".into());
            }
            MssqlSchedulerFieldKind::UnqualifiedCounter { .. } => {
                sample.properties.insert(field.name.into(), "0".into());
            }
            MssqlSchedulerFieldKind::Counter { unit, nature } => sample.counters.push(NativeCounterSample {
                key: field.name.into(),
                value: MeasurementValue::Integer("9007199254740993".into()),
                unit,
                nature,
                scope: crate::MeasurementScope::Server,
                reset: crate::ResetMarker::Unknown,
                native_object: MSSQL_SCHEDULER_OBJECT.into(),
                native_name: field.name.into(),
                native_instance: None,
                native_type: None,
            }),
        }
    }
    sample
}
fn field(name: &str) -> &'static MssqlSchedulerField {
    MSSQL_SCHEDULER_FIELDS.iter().find(|field| field.name == name).unwrap()
}

#[test]
fn native_scheduler_fields_preserve_exact_integer_values_and_do_not_create_reset_or_rate_proof() {
    let sample = sample();
    let point = MssqlSchedulerPoint::from_sample(&sample).unwrap();
    assert_eq!(point.scheduler_id(), "0");
    assert!(matches!(
        point.field(field("work_queue_count")).unwrap(),
        MssqlSchedulerValue::Observed("9007199254740993")
    ));
    assert!(matches!(point.field(field("is_idle")).unwrap(), MssqlSchedulerValue::Observed("0")));
    assert!(sample.counters.iter().all(|counter| counter.reset == crate::ResetMarker::Unknown));
    let bytes = serde_json::to_vec(&sample).unwrap();
    let restored = serde_json::from_slice(&bytes).unwrap();
    assert!(MssqlSchedulerPoint::from_sample(&restored).is_ok());
}

#[test]
fn native_null_and_unavailable_columns_are_distinct_from_observed_zero() {
    let mut sample = sample();
    sample.properties.remove("failed_to_create_worker");
    sample.properties.insert("native_null_columns".into(), "[\"failed_to_create_worker\"]".into());
    sample.counters.retain(|counter| counter.key != "ideal_workers_limit");
    sample.properties.insert("unavailable_columns".into(), "[\"ideal_workers_limit\"]".into());
    let point = MssqlSchedulerPoint::from_sample(&sample).unwrap();
    assert!(matches!(point.field(field("failed_to_create_worker")).unwrap(), MssqlSchedulerValue::NativeNull));
    assert!(matches!(point.field(field("ideal_workers_limit")).unwrap(), MssqlSchedulerValue::ColumnUnavailable));
    assert!(matches!(point.field(field("is_online")).unwrap(), MssqlSchedulerValue::Observed("0")));
}

#[test]
fn counter_units_nature_reset_native_object_and_exact_integer_semantics_are_not_inferred_from_a_name() {
    for mutation in [
        "unit",
        "nature",
        "reset",
        "object",
        "name",
        "instance",
        "type",
        "float",
        "fraction",
        "negative",
        "leading-zero",
    ] {
        let mut sample = sample();
        let counter = sample.counters.iter_mut().find(|counter| counter.key == "work_queue_count").unwrap();
        match mutation {
            "unit" => counter.unit = CounterUnit::Milliseconds,
            "nature" => counter.nature = CounterNature::Cumulative,
            "reset" => counter.reset = crate::ResetMarker::Generation("unproved".into()),
            "object" => counter.native_object = "another-native-view".into(),
            "name" => counter.native_name = "another-field".into(),
            "instance" => counter.native_instance = Some("another-owner".into()),
            "type" => counter.native_type = Some("65792".into()),
            "float" => counter.value = MeasurementValue::Real(1.0),
            "fraction" => counter.value = MeasurementValue::Integer("1.00000000000001".into()),
            "negative" => counter.value = MeasurementValue::Integer("-1".into()),
            "leading-zero" => counter.value = MeasurementValue::Integer("01".into()),
            _ => unreachable!(),
        }
        assert!(MssqlSchedulerPoint::from_sample(&sample).is_err(), "{mutation}");
    }
}

#[test]
fn missing_conflicting_duplicate_or_unknown_field_representations_refuse_the_point() {
    for mutation in [
        "missing",
        "null-real",
        "double",
        "conflict",
        "duplicate-null",
        "unknown-null",
        "required-unavailable",
        "bad-boolean",
        "control-status",
        "unknown-property",
        "unknown-counter",
        "foreign-scope",
        "qualifier",
        "noncanonical-id",
    ] {
        let mut sample = sample();
        match mutation {
            "missing" => {
                sample.properties.remove("is_online");
            }
            "null-real" => {
                sample.counters[0].value = MeasurementValue::Real(1.0);
                sample.properties.insert("native_null_columns".into(), format!("[\"{}\"]", sample.counters[0].key));
            }
            "double" => sample.counters.push(sample.counters[0].clone()),
            "conflict" => {
                sample.properties.insert("native_null_columns".into(), "[\"is_online\"]".into());
            }
            "duplicate-null" => {
                sample.properties.remove("is_online");
                sample.properties.insert("native_null_columns".into(), "[\"is_online\",\"is_online\"]".into());
            }
            "unknown-null" => {
                sample.properties.insert("native_null_columns".into(), "[\"unknown\"]".into());
            }
            "required-unavailable" => {
                sample.properties.remove("status");
                sample.properties.insert("unavailable_columns".into(), "[\"status\"]".into());
            }
            "bad-boolean" => {
                sample.properties.insert("is_idle".into(), "true".into());
            }
            "control-status" => {
                sample.properties.insert("status".into(), "VISIBLE\nONLINE".into());
            }
            "unknown-property" => {
                sample.properties.insert("sql_text".into(), "private SQL canary".into());
            }
            "unknown-counter" => {
                sample.counters[0].key = "undocumented".into();
            }
            "foreign-scope" => {
                sample.scope = crate::MeasurementScope::Database;
                for counter in &mut sample.counters {
                    counter.scope = sample.scope;
                }
            }
            "qualifier" => {
                sample.identity.qualifiers.insert("database".into(), "private DB canary".into());
            }
            "noncanonical-id" => sample.identity.id = "00".into(),
            _ => unreachable!(),
        }
        assert!(MssqlSchedulerPoint::from_sample(&sample).is_err(), "{mutation}");
    }
}

#[test]
fn modern_native_semantics_are_typed_without_turning_spinlock_raw_values_into_measurements() {
    let sample = sample();
    let point = MssqlSchedulerPoint::from_sample(&sample).unwrap();
    assert_eq!(MSSQL_SCHEDULER_FIELDS.len(), 30);
    for name in [
        "queued_disk_io_count",
        "total_enqueued_tasks",
        "total_signal_time",
        "total_waits_completed",
        "total_completed_tasks",
    ] {
        assert!(matches!(point.field(field(name)).unwrap(), MssqlSchedulerValue::Observed("9007199254740993")));
    }
    for name in ["spinlock_wait_time_ms", "spinlock_max_wait_time_ms", "spinlock_wait_count"] {
        assert!(matches!(
            point.field(field(name)).unwrap(),
            MssqlSchedulerValue::Unqualified { native_value: "0", required_global_trace_flag: 8134 }
        ));
        assert!(!sample.counters.iter().any(|counter| counter.key == name));
    }
}

#[test]
fn catalog_absence_native_null_and_raw_unknown_instrumentation_are_independent_states() {
    let mut sample = sample();
    sample.properties.remove("spinlock_wait_time_ms");
    sample.properties.insert("native_null_columns".into(), "[\"spinlock_wait_time_ms\"]".into());
    sample.properties.remove("spinlock_max_wait_time_ms");
    sample.properties.insert("unavailable_columns".into(), "[\"spinlock_max_wait_time_ms\"]".into());
    let point = MssqlSchedulerPoint::from_sample(&sample).unwrap();
    assert!(matches!(point.field(field("spinlock_wait_time_ms")).unwrap(), MssqlSchedulerValue::NativeNull));
    assert!(matches!(point.field(field("spinlock_max_wait_time_ms")).unwrap(), MssqlSchedulerValue::ColumnUnavailable));
    assert!(matches!(
        point.field(field("spinlock_wait_count")).unwrap(),
        MssqlSchedulerValue::Unqualified { native_value: "0", .. }
    ));
}

#[test]
fn diagnostic_properties_cannot_self_assert_enabled_instrumentation_or_become_a_scalar_counter() {
    for mutation in ["enabled-property", "counter", "negative", "noncanonical", "conflicting-null"] {
        let mut sample = sample();
        match mutation {
            "enabled-property" => {
                sample.properties.insert("trace_flag_8134_enabled".into(), "1".into());
            }
            "counter" => {
                sample.properties.remove("spinlock_wait_count");
                sample.counters.push(NativeCounterSample::gauge(
                    "spinlock_wait_count",
                    0,
                    CounterUnit::Count,
                    crate::MeasurementScope::Server,
                    MSSQL_SCHEDULER_OBJECT,
                    "spinlock_wait_count",
                ));
            }
            "negative" => {
                sample.properties.insert("spinlock_wait_count".into(), "-1".into());
            }
            "noncanonical" => {
                sample.properties.insert("spinlock_wait_count".into(), "00".into());
            }
            "conflicting-null" => {
                sample.properties.insert("native_null_columns".into(), "[\"spinlock_wait_count\"]".into());
            }
            _ => unreachable!(),
        }
        assert!(MssqlSchedulerPoint::from_sample(&sample).is_err(), "{mutation}");
    }
}
