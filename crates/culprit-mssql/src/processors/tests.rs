use super::*;
use crate::{
    resources::test_support::{FakeRead, replace},
    value::{Rec, Val},
};
use culprit_core::ResetMarker;

pub(crate) fn row() -> Rec {
    Rec::of(&[("host_processors", Val::Int(64)), ("online_schedulers", Val::Int(8)), ("numa_nodes", Val::Int(2))])
}
#[tokio::test]
async fn native_host_capacity_is_distinct_from_online_sql_user_schedulers() {
    let mut reader = FakeRead::new(vec![Ok(vec![row()])]);
    let topology = read(&mut reader).await.unwrap();
    assert_eq!(topology.application(), (Some(8.0), Some(2.0)));
    let observations = topology.observations();
    assert_eq!(observations.len(), 3);
    assert!(observations.iter().all(OperationalObservation::is_valid));
    let counter = |index: usize| {
        let OperationalFact::Counter(value) = &observations[index].fact else { panic!("counter") };
        value
    };
    assert_eq!(counter(0).value, culprit_core::telemetry::MeasurementValue::Integer("64".into()));
    assert_eq!(counter(1).value, culprit_core::telemetry::MeasurementValue::Integer("8".into()));
    assert_eq!(counter(2).value, culprit_core::telemetry::MeasurementValue::Integer("2".into()));
    assert!(observations.iter().all(|row|matches!(&row.fact,OperationalFact::Counter(value) if value.scope==MeasurementScope::Server && value.nature==CounterNature::Gauge && value.unit==CounterUnit::Count && value.reset==ResetMarker::Unknown)));
}
#[tokio::test]
async fn no_visible_schedulers_does_not_become_an_available_zero_processor_configuration() {
    let mut reader = FakeRead::new(vec![Ok(vec![replace(
        &row(),
        &[("online_schedulers", Val::Int(0)), ("numa_nodes", Val::Int(0))],
    )])]);
    let topology = read(&mut reader).await.unwrap();
    assert_eq!(topology.application(), (None, None));
    let counters = topology.observations();
    let OperationalFact::Counter(value) = &counters[1].fact else { panic!("counter") };
    assert_eq!(
        value.value,
        culprit_core::telemetry::MeasurementValue::Integer("0".into()),
        "the observed native visibility count stays exact; the app's processor capacity is unknown"
    );
}
#[tokio::test]
async fn unavailable_or_malformed_topology_never_becomes_an_empty_or_guessed_single_node() {
    for result in [
        Ok(vec![]),
        Ok(vec![row(), row()]),
        Ok(vec![replace(&row(), &[("host_processors", Val::Null)])]),
        Ok(vec![replace(&row(), &[("numa_nodes", Val::Int(9))])]),
        Err(ReadError::Stopped(CollectionStop::TimedOut)),
    ] {
        let mut reader = FakeRead::new(vec![result]);
        assert!(read(&mut reader).await.is_err());
        assert_eq!(reader.calls.len(), 1);
    }
}
