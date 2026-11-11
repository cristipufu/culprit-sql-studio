use super::*;
use crate::{
    resources::test_support::{FakeRead, replace},
    value::{Rec, Val},
};

pub(crate) fn row() -> Rec {
    Rec::of(&[
        ("total_plans", Val::Int(3)),
        ("known_sizes", Val::Int(3)),
        ("total_bytes", Val::Int(9_007_199_254_740_993)),
        ("adhoc_lookup_one_entries", Val::Int(1)),
        ("prepared_lookup_one_entries", Val::Int(2)),
        ("adhoc_lookup_one_bytes", Val::Int(1024)),
        ("prepared_lookup_one_bytes", Val::Int(9_007_199_254_739_969)),
    ])
}
#[tokio::test]
async fn cache_entry_counts_and_bytes_keep_precision_scope_and_the_same_app_projection() {
    let mut reader = FakeRead::new(vec![Ok(vec![row()])]);
    let summary = read(&mut reader).await.unwrap();
    assert_eq!(summary.application().total_plans, 3.0);
    assert_eq!(summary.application().single_use_bytes, 1024.0);
    assert_eq!(summary.application().single_use_plans, 1.0);
    assert_eq!(summary.prepared_lookup_one_bytes, 9_007_199_254_739_969);
    let facts = summary.observations();
    assert_eq!(facts.len(), 6);
    assert!(facts.iter().all(OperationalObservation::is_valid));
    let OperationalFact::Counter(bytes) = &facts[1].fact else { panic!("counter") };
    assert_eq!(bytes.value, culprit_core::telemetry::MeasurementValue::Integer("9007199254740993".into()));
    assert_eq!(bytes.unit, CounterUnit::Bytes);
    assert_eq!(bytes.scope, MeasurementScope::Database);
    assert_eq!(bytes.nature, CounterNature::Gauge);
    assert_eq!(bytes.reset, culprit_core::ResetMarker::Unknown);
}
#[tokio::test]
async fn only_a_confirmed_empty_cache_turns_native_sum_nulls_into_zero() {
    let empty = Rec::of(&[
        ("total_plans", Val::Int(0)),
        ("known_sizes", Val::Int(0)),
        ("total_bytes", Val::Null),
        ("adhoc_lookup_one_entries", Val::Null),
        ("prepared_lookup_one_entries", Val::Null),
        ("adhoc_lookup_one_bytes", Val::Null),
        ("prepared_lookup_one_bytes", Val::Null),
    ]);
    let mut reader = FakeRead::new(vec![Ok(vec![empty])]);
    let summary = read(&mut reader).await.unwrap();
    assert_eq!(summary.application(), PlanCacheUse::default());
    assert_eq!(summary.observations().len(), 6);
    for invalid in [
        replace(&row(), &[("known_sizes", Val::Int(2))]),
        replace(&row(), &[("total_bytes", Val::Null)]),
        replace(&row(), &[("adhoc_lookup_one_bytes", Val::Int(-1))]),
        replace(&row(), &[("adhoc_lookup_one_entries", Val::Int(4))]),
        replace(&row(), &[("prepared_lookup_one_bytes", Val::Null)]),
        replace(&row(), &[("prepared_lookup_one_entries", Val::Int(3))]),
        replace(&row(), &[("prepared_lookup_one_bytes", Val::Int(9_007_199_254_740_993))]),
    ] {
        assert!(read(&mut FakeRead::new(vec![Ok(vec![invalid])])).await.is_err());
    }
}
#[tokio::test]
async fn failed_or_missing_cache_aggregates_are_unavailable_without_an_empty_fallback_or_retry() {
    for result in [Ok(vec![]), Ok(vec![row(), row()]), Err(ReadError::Stopped(CollectionStop::TimedOut))] {
        let mut reader = FakeRead::new(vec![result]);
        assert!(read(&mut reader).await.is_err());
        assert_eq!(reader.calls.len(), 1);
    }
}
