use super::*;
use crate::{Engine, MeasurementScope, ResetMarker, SourceState};
use std::time::{Duration, SystemTime};

fn row() -> WaitCounterObservation {
    WaitCounterObservation {
        source: crate::source_id::WAITS.into(),
        wait_type: "SLEEP_TASK".into(),
        mechanism: None,
        scope: MeasurementScope::Server,
        reset: ResetMarker::Unknown,
        waiting_tasks_count: "9007199254740993".into(),
        wait_time_ms: "18446744073709551615".into(),
        signal_wait_time_ms: "9007199254740993".into(),
        max_wait_time_ms: "7".into(),
    }
}
fn read() -> WaitCounterSnapshot {
    WaitCounterSnapshot {
        context: Some(
            ObservationReadContext::collector(SystemTime::UNIX_EPOCH, SystemTime::UNIX_EPOCH + Duration::from_secs(1))
                .unwrap(),
        ),
        sources: vec![StateSourceCoverage::collected(crate::source_id::WAITS.into(), 1)],
        observations: vec![row()],
        read_complete: true,
    }
}
fn source() -> CollectionDescription {
    CollectionDescription {
        target: DatabaseIdentity::server(Engine::Mssql, "db", "owned.invalid", 1433, None).into(),
        source: "logical-wait-recipe".into(),
        partition: SourcePartition::Target,
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::Snapshot(StateSnapshotDescription {
            dataset: SnapshotDataset::WaitCounters,
            observed_from: "2020-01-01T00:00:00.000000001Z".into(),
            observed_until: "2020-01-01T00:00:00.000000002Z".into(),
            records: 1,
            sources: read().sources,
        }),
    }
}

#[test]
fn exact_counts_and_checked_resource_time_survive_roundtrip_without_a_native_clock_or_lifetime() {
    let original = read();
    assert!(original.is_valid_for(Engine::Mssql));
    assert!(original.context.as_ref().unwrap().native().is_none());
    let decoded: WaitCounterSnapshot = serde_json::from_slice(&serde_json::to_vec(&original).unwrap()).unwrap();
    assert_eq!(original, decoded);
    assert_eq!(decoded.observations[0].waiting_tasks_count, "9007199254740993");
    assert_eq!(decoded.observations[0].resource_wait_time_ms().as_deref(), Some("18437736874454810622"));
    assert_eq!(decoded.observations[0].reset, ResetMarker::Unknown);
}

#[test]
fn contradictory_raw_signal_is_preserved_without_a_clamped_or_invented_resource_time() {
    let mut read = read();
    read.observations[0].wait_time_ms = "1".into();
    read.observations[0].signal_wait_time_ms = "2".into();
    assert!(read.is_valid());
    assert_eq!(read.observations[0].resource_wait_time_ms(), None);
    read.observations[0].wait_time_ms = "18446744073709551616".into();
    assert!(!read.is_valid());
    assert_eq!(read.observations[0].resource_wait_time_ms(), None);
}

#[test]
fn duplicate_canonical_source_and_wait_type_cannot_gain_identity_from_changed_scope_or_reset() {
    for change in ["scope", "reset", "mechanism"] {
        let mut read = read();
        let mut another = read.observations[0].clone();
        match change {
            "scope" => another.scope = MeasurementScope::Database,
            "reset" => another.reset = ResetMarker::Generation("other".into()),
            _ => another.mechanism = Engine::Mssql.spec().native_wait_mechanism("RBIO_RG_STORAGE"),
        }
        read.observations.push(another);
        read.sources[0].records = 2;
        assert!(!read.is_valid(), "{change}");
    }
}

#[test]
fn native_declared_population_body_gaps_and_unavailable_empty_sources_remain_independent() {
    let source = source();
    let mut read = WaitCounterSnapshot::from_source(&source, vec![], false).unwrap();
    assert_eq!(read.sources[0].records, 1);
    assert_eq!(read.context.as_ref().unwrap().native().unwrap().target, source.target);
    read.read_complete = true;
    assert!(!read.is_valid());
    read.sources[0] =
        StateSourceCoverage::unavailable(crate::source_id::WAITS.into(), ErrorCode::PermissionDenied, None);
    assert!(read.is_valid());
    assert_eq!(read.sources[0].state, SourceState::Unavailable);
    read.observations.push(row());
    assert!(!read.is_valid());
    read.observations.clear();
    read.sources[0] = StateSourceCoverage::collected(crate::source_id::WAITS.into(), 0);
    assert!(read.is_valid());
    assert_eq!(read.sources[0].state, SourceState::Empty);
}

#[test]
fn foreign_headers_sources_and_native_mechanisms_fail_without_repair() {
    let mut source = source();
    let SourceObservation::Snapshot(frame) = &mut source.observation else { panic!() };
    frame.dataset = SnapshotDataset::Operational;
    assert!(WaitCounterSnapshot::from_source(&source, vec![row()], true).is_err());
    let mut read = read();
    read.observations[0].source = "foreign".into();
    assert!(!read.is_valid());
    read.observations[0] = row();
    read.observations[0].wait_type = "RBIO_RG_STORAGE".into();
    assert!(!read.is_valid_for(Engine::Mssql));
    read.observations[0].mechanism = Engine::Mssql.spec().native_wait_mechanism("RBIO_RG_STORAGE");
    assert!(read.is_valid_for(Engine::Mssql));
    assert!(!read.is_valid_for(Engine::Postgres));
    read.sources.push(read.sources[0].clone());
    assert!(!read.is_valid());
}

#[test]
fn named_population_and_source_bounds_fail_closed_without_discarding_a_declared_partial_body() {
    let mut read = read();
    read.sources[0].records = MAX_WAIT_COUNTER_OBSERVATIONS as u32 + 1;
    read.read_complete = false;
    assert!(!read.is_valid());
    read.sources[0].records = 1;
    read.context = Some(ObservationReadContext::Collector {
        observed_from: "2020-01-01T00:00:01Z".into(),
        observed_until: "2020-01-01T00:00:00Z".into(),
    });
    assert!(!read.is_valid());
    read.context = None;
    assert!(read.is_valid());
    read.sources[0].state = SourceState::Partial;
    read.sources[0].complete_scope = false;
    read.observations.clear();
    assert!(read.is_valid());
    assert_eq!(read.sources[0].records, 1);
}
