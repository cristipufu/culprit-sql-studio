use super::*;
use crate::{
    resources::test_support::{FakeRead, replace},
    value::{Rec, Val},
};

pub(crate) fn row() -> Rec {
    Rec::of(&[
        ("pool_id", Val::Int(2)),
        ("name", Val::Text("Big Gateway".into())),
        ("max_count", Val::Int(1)),
        ("active_count", Val::Int(1)),
        ("waiter_count", Val::Int(2)),
        ("threshold_factor", Val::Int(8)),
        ("threshold", Val::Int(9_007_199_254_740_993)),
        ("is_active", Val::Bool(true)),
    ])
}

#[tokio::test]
async fn shared_gateway_policy_is_exact_and_never_becomes_a_database_allocation_or_a_query_owner() {
    let bypassed =
        replace(&row(), &[("pool_id", Val::Int(3)), ("threshold", Val::Int(-1)), ("is_active", Val::Bool(false))]);
    let mut reader = FakeRead::new(vec![Ok(vec![row(), bypassed])]);
    let samples = read(&mut reader, 10).await.unwrap();
    let first = samples[0].observation().unwrap();
    let second = samples[1].observation().unwrap();
    assert!(first.is_valid() && second.is_valid());
    assert_ne!(first.key, second.key, "different native pools cannot share observation identity");
    let OperationalFact::CounterSet(counters) = &first.fact else { panic!("gateway counters") };
    assert_eq!(counters.scope, MeasurementScope::Server);
    assert_eq!(counters.identity.qualifiers["resource_pool_id"], "2");
    assert_eq!(counters.properties["threshold_factor"], "8");
    let threshold = counters.counters.iter().find(|counter| counter.native_name == "threshold").unwrap();
    assert_eq!(threshold.unit, CounterUnit::Bytes);
    assert_eq!(threshold.value, MeasurementValue::Integer("9007199254740993".into()));
    let OperationalFact::CounterSet(counters) = second.fact else { panic!("gateway counters") };
    assert_eq!(counters.properties["threshold"], "-1");
    assert_eq!(counters.properties["is_active"], "0");
    assert!(!counters.counters.iter().any(|counter| counter.native_name == "threshold"));
    let signal = samples[0].waiting_signal().unwrap().unwrap();
    assert_eq!(signal.measurement_scope, Some(MeasurementScope::Server));
    assert_eq!(signal.status, culprit_core::SignalStatus::Info);
    assert_eq!(signal.value, 2.0);
    assert!(signal.healthy.unwrap().contains("ownership"));
    assert_eq!(reader.calls.len(), 1);
}

#[tokio::test]
async fn truncated_duplicate_invalid_and_failed_gateway_reads_never_publish_partial_or_healthy_counts() {
    for reply in [
        Ok(vec![row(), row()]),
        Ok(vec![replace(&row(), &[("threshold", Val::Int(-2))])]),
        Ok(vec![replace(&row(), &[("active_count", Val::Null)])]),
        Ok(vec![replace(&row(), &[("waiter_count", Val::Int(-1))])]),
        Ok(vec![replace(&row(), &[("is_active", Val::Int(2))])]),
        Err(ReadError::Stopped(CollectionStop::TimedOut)),
    ] {
        let mut reader = FakeRead::new(vec![reply]);
        assert!(read(&mut reader, 10).await.is_err());
        assert_eq!(reader.calls.len(), 1, "there is no retry or unbounded fallback");
    }
    let other = replace(&row(), &[("pool_id", Val::Int(3))]);
    let mut reader = FakeRead::new(vec![Ok(vec![row(), other])]);
    assert!(read(&mut reader, 1).await.is_err());
    assert_eq!(reader.calls[0].limit, 2, "read one overflow witness, not an unbounded result");
}

#[tokio::test]
async fn idle_gateways_remain_exportable_without_adding_health_rows_and_counts_do_not_change_identity() {
    let changed = replace(&row(), &[("active_count", Val::Int(0)), ("waiter_count", Val::Int(0))]);
    let mut reader = FakeRead::new(vec![Ok(vec![row()]), Ok(vec![changed])]);
    let first = read(&mut reader, 10).await.unwrap().remove(0);
    let second = read(&mut reader, 10).await.unwrap().remove(0);
    assert_eq!(first.observation().unwrap().key, second.observation().unwrap().key);
    assert!(second.waiting_signal().unwrap().is_none());
    assert!(second.observation().unwrap().is_valid());
}
