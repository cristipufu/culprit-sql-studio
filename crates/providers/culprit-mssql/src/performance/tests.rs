use super::*;
use crate::{
    resources::test_support::{FakeRead, replace},
    value::{Rec, Val},
};
pub(crate) fn row() -> Rec {
    Rec::of(&[
        ("native_object", Val::Text("SQLServer:SQL Statistics".into())),
        ("native_name", Val::Text("Batch Requests/sec".into())),
        ("native_instance", Val::Text("".into())),
        ("native_type", Val::Int(272_696_576)),
        ("value", Val::Int(9_007_199_254_740_993)),
    ])
}
#[tokio::test]
async fn native_meaning_scope_and_precision_survive_application_projection_and_export() {
    let gauge = replace(
        &row(),
        &[
            ("native_object", Val::Text("SQLServer:Buffer Manager".into())),
            ("native_name", Val::Text("Page life expectancy".into())),
            ("native_type", Val::Int(65_792)),
        ],
    );
    let mut reader = FakeRead::new(vec![Ok(vec![row(), gauge])]);
    let facts = read(&mut reader, 32).await.unwrap();
    assert_eq!(facts[0].value, culprit_core::telemetry::MeasurementValue::Integer("9007199254740993".into()));
    assert_eq!(facts[0].nature, CounterNature::Cumulative);
    assert_eq!(facts[1].nature, CounterNature::Gauge);
    assert_eq!(facts[1].unit, CounterUnit::Seconds);
    assert!(facts.iter().all(|row| row.reset == ResetMarker::Unknown && row.scope == MeasurementScope::Server));
    assert_eq!(application(&facts).len(), 2);
    let exported = observations(facts).unwrap();
    assert_ne!(exported[0].key, exported[1].key);
    assert!(exported.iter().all(|row| row.is_valid()));
}
#[tokio::test]
async fn unknown_or_unexpected_counter_types_keep_raw_values_without_inventing_known_meaning() {
    for code in [42, 65_792, -2_147_483_648] {
        let mut reader = FakeRead::new(vec![Ok(vec![replace(&row(), &[("native_type", Val::Int(code))])])]);
        let facts = read(&mut reader, 32).await.unwrap();
        assert_eq!(facts[0].nature, CounterNature::Unknown);
        assert_eq!(facts[0].unit, CounterUnit::Unknown);
        assert_eq!(facts[0].value, culprit_core::telemetry::MeasurementValue::Integer("9007199254740993".into()));
        assert!(application(&facts).is_empty());
    }
}
#[tokio::test]
async fn instance_rows_never_replace_or_double_count_global_application_counters() {
    let instance = replace(&row(), &[("native_instance", Val::Text("another-database".into()))]);
    let mut reader = FakeRead::new(vec![Ok(vec![row(), instance])]);
    let facts = read(&mut reader, 32).await.unwrap();
    assert_eq!(facts.len(), 2);
    assert_eq!(application(&facts).len(), 1);
    let exported = observations(facts).unwrap();
    assert_ne!(exported[0].key, exported[1].key);
}
#[tokio::test]
async fn empty_disabled_counters_limits_duplicate_ids_and_malformed_values_are_not_measured_zeroes() {
    for (limit, rows) in [
        (32, vec![]),
        (1, vec![row(), row()]),
        (32, vec![row(), row()]),
        (32, vec![replace(&row(), &[("value", Val::Int(-1))])]),
        (32, vec![replace(&row(), &[("value", Val::Null)])]),
        (32, vec![replace(&row(), &[("native_instance", Val::Null)])]),
    ] {
        assert!(read(&mut FakeRead::new(vec![Ok(rows)]), limit).await.is_err());
    }
}
#[tokio::test]
async fn sampled_rates_subtract_exact_values_and_skip_missing_reset_or_changed_counter_bases() {
    let before = read(&mut FakeRead::new(vec![Ok(vec![row()])]), 32).await.unwrap();
    let mut after = before.clone();
    after[0].value = culprit_core::telemetry::MeasurementValue::Integer("9007199254740994".into());
    assert_eq!(sampled_rates(&before, &after, 2.0)["Batch Requests/sec"], 0.5);
    assert!(sampled_rates(&[], &after, 2.0).is_empty());
    let mut reset = after.clone();
    reset[0].value = culprit_core::telemetry::MeasurementValue::Integer("0".into());
    assert!(sampled_rates(&before, &reset, 2.0).is_empty());
    let mut changed = after;
    changed[0].native_object = "another-instance:SQL Statistics".into();
    assert!(sampled_rates(&before, &changed, 2.0).is_empty());
}
