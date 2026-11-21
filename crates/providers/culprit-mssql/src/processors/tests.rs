use super::*;
use crate::{
    resources::test_support::{FakeRead, replace},
    value::{Rec, Val},
};
use culprit_core::ResetMarker;

pub(crate) fn row() -> Rec {
    Rec::of(&[
        ("host_processors", Val::Int(64)),
        ("online_schedulers", Val::Int(8)),
        ("numa_nodes", Val::Int(2)),
        ("edition", Val::Int(3)),
        ("major", Val::Text("16".into())),
        ("server_name", Val::Text("fixture-machine\\NATIVE".into())),
        ("machine_name", Val::Text("fixture-machine".into())),
        ("process_id", Val::Int(42)),
        ("observed_at", Val::Text("2020-01-01T06:00:00Z".into())),
        ("started_at_ticks", Val::Int(100)),
        ("ms_ticks", Val::Int(500)),
        ("process_user_time_ms", Val::Int(9_007_199_254_740_993)),
        ("process_kernel_time_ms", Val::Int(0)),
    ])
}
#[tokio::test]
async fn native_host_capacity_is_distinct_from_online_sql_user_schedulers() {
    let mut reader = FakeRead::new(vec![Ok(vec![row()])]);
    let topology = read(&mut reader, true).await.unwrap();
    assert_eq!(topology.application(), (Some(8.0), Some(2.0)));
    let observations = topology.observations().unwrap();
    assert_eq!(observations.len(), 4);
    assert!(observations.iter().all(OperationalObservation::is_valid));
    let counter = |index: usize| {
        let OperationalFact::Counter(value) = &observations[index].fact else { panic!("counter") };
        value
    };
    assert_eq!(counter(0).value, culprit_core::telemetry::MeasurementValue::Integer("64".into()));
    assert_eq!(counter(1).value, culprit_core::telemetry::MeasurementValue::Integer("8".into()));
    assert_eq!(counter(2).value, culprit_core::telemetry::MeasurementValue::Integer("2".into()));
    assert!(observations[..3].iter().all(|row|matches!(&row.fact,OperationalFact::Counter(value) if value.scope==MeasurementScope::Server && value.nature==CounterNature::Gauge && value.unit==CounterUnit::Count && value.reset==ResetMarker::Unknown)));
}
#[tokio::test]
async fn no_visible_schedulers_does_not_become_an_available_zero_processor_configuration() {
    let mut reader = FakeRead::new(vec![Ok(vec![replace(
        &row(),
        &[("online_schedulers", Val::Int(0)), ("numa_nodes", Val::Int(0))],
    )])]);
    let topology = read(&mut reader, true).await.unwrap();
    assert_eq!(topology.application(), (None, None));
    let counters = topology.observations().unwrap();
    let OperationalFact::Counter(value) = &counters[1].fact else { panic!("counter") };
    assert_eq!(
        value.value,
        culprit_core::telemetry::MeasurementValue::Integer("0".into()),
        "the observed native visibility count stays exact; the app's processor capacity is unknown"
    );
}

#[tokio::test]
async fn same_singleton_retains_exact_cpu_totals_native_ticks_and_original_process_without_another_query() {
    let mut reader = FakeRead::new(vec![Ok(vec![row()])]);
    let topology = read(&mut reader, true).await.unwrap();
    let sample = topology.into_cpu_counters().unwrap();
    let point = MssqlProcessCpuPoint::from_sample(&sample).unwrap();
    assert_eq!((point.user_ms(), point.kernel_ms()), (Some("9007199254740993"), Some("0")));
    assert_eq!((point.process_id(), point.started_ticks(), point.sampled_ticks()), ("42", "100", "500"));
    assert_eq!(point.observed_at(), "2020-01-01T06:00:00.000000000Z");
    assert!(sample.counters.iter().all(|value| value.nature == CounterNature::Cumulative
        && value.unit == CounterUnit::Milliseconds
        && value.reset == ResetMarker::Generation(sample.identity.qualifiers["instance_epoch"].clone())));
    assert_eq!(reader.calls.len(), 1);
    assert_eq!(reader.calls[0].limit, 2);
    assert!(reader.calls[0].parameters.is_empty());
    assert!(reader.calls[0].sql.contains("i.process_user_time_ms,i.process_kernel_time_ms"));
    assert!(!reader.calls[0].sql.contains("ring_buffers"));
}

#[tokio::test]
async fn native_null_cpu_totals_preserve_topology_and_never_become_measured_zero() {
    for (user, kernel) in [(Val::Null, Val::Int(0)), (Val::Null, Val::Null)] {
        let mut reader = FakeRead::new(vec![Ok(vec![replace(
            &row(),
            &[("process_user_time_ms", user), ("process_kernel_time_ms", kernel)],
        )])]);
        let topology = read(&mut reader, true).await.unwrap();
        assert_eq!(topology.application(), (Some(8.0), Some(2.0)));
        let sample = topology.into_cpu_counters().unwrap();
        let point = MssqlProcessCpuPoint::from_sample(&sample).unwrap();
        assert_eq!(point.user_ms(), None);
        assert!(point.kernel_ms().is_none_or(|value| value == "0"));
    }
}

#[tokio::test]
async fn unqualified_azure_instance_never_borrows_boxed_process_authority() {
    let mut reader = FakeRead::new(vec![Ok(vec![replace(&row(), &[("edition", Val::Int(5))])])]);
    let topology = read(&mut reader, false).await.unwrap();
    assert_eq!(topology.application(), (Some(8.0), Some(2.0)));
    assert_eq!(topology.observations().unwrap().len(), 3);
    assert!(topology.into_cpu_counters().is_none());
    assert_eq!(reader.calls.len(), 1);
    assert!(!reader.calls[0].sql.contains("process_user_time_ms"));
    assert!(!reader.calls[0].sql.contains("SERVERPROPERTY"));
}

#[tokio::test]
async fn native_null_process_authority_preserves_independent_topology_counts() {
    for column in ["process_id", "started_at_ticks", "ms_ticks", "server_name", "major"] {
        let mut reader = FakeRead::new(vec![Ok(vec![replace(&row(), &[(column, Val::Null)])])]);
        let topology = read(&mut reader, true).await.unwrap();
        assert_eq!(topology.application(), (Some(8.0), Some(2.0)), "{column}");
        assert!(topology.into_cpu_counters().is_none(), "{column}");
        assert_eq!(reader.calls.len(), 1);
    }
}

#[tokio::test]
async fn restart_changes_the_native_counter_generation_without_fabricating_a_delta() {
    let mut identities = Vec::new();
    for started in [100, 101] {
        let mut reader = FakeRead::new(vec![Ok(vec![replace(&row(), &[("started_at_ticks", Val::Int(started))])])]);
        identities.push(read(&mut reader, true).await.unwrap().into_cpu_counters().unwrap());
    }
    assert_ne!(identities[0].identity, identities[1].identity);
    assert_ne!(identities[0].counters[0].reset, identities[1].counters[0].reset);
}

#[tokio::test]
async fn malformed_cpu_or_inverted_ticks_refuses_the_point_without_fallback_queries() {
    for changes in [
        vec![("process_user_time_ms", Val::Int(-1))],
        vec![("process_kernel_time_ms", Val::Text("invalid".into()))],
        vec![("started_at_ticks", Val::Int(501))],
    ] {
        let mut reader = FakeRead::new(vec![Ok(vec![replace(&row(), &changes)])]);
        assert!(read(&mut reader, true).await.is_err());
        assert_eq!(reader.calls.len(), 1);
    }
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
        assert!(read(&mut reader, true).await.is_err());
        assert_eq!(reader.calls.len(), 1);
    }
}
