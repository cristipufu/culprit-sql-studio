use super::*;
use crate::{Engine, MeasurementScope, SourceState};
use std::time::Duration;

fn point(source: &str) -> ReplicationObservation {
    let set = NativeCounterSet {
        identity: NativeIdentity::new("slot-a"),
        scope: MeasurementScope::Server,
        properties: BTreeMap::from([
            ("kind".into(), "slot".into()),
            ("inactive_since".into(), "2026-11-16T12:00:00Z".into()),
        ]),
        counters: vec![NativeCounterSample::gauge(
            "kept",
            9_007_199_254_740_993,
            CounterUnit::Bytes,
            MeasurementScope::Server,
            source,
            "kept",
        )],
    };
    ReplicationObservation {
        source: source.into(),
        fact: ReplicationFact::Native(Box::new(NativeReplicationSample::new(set).unwrap())),
    }
}

fn snapshot() -> ReplicationSnapshot {
    ReplicationSnapshot {
        context: ReplicationReadContext::collector(
            SystemTime::UNIX_EPOCH,
            SystemTime::UNIX_EPOCH + Duration::from_secs(1),
        )
        .unwrap(),
        sources: vec![StateSourceCoverage::observed("pg_replication_slots".into(), 1, false)],
        observations: vec![point("pg_replication_slots")],
    }
}

fn source() -> CollectionDescription {
    CollectionDescription {
        target: DatabaseIdentity::server(Engine::Postgres, "db", "owned.invalid", 5432, None).into(),
        partition: SourcePartition::Target,
        source: "replication-state".into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::Snapshot(StateSnapshotDescription {
            dataset: SnapshotDataset::Replication,
            observed_from: "2020-01-01T00:00:00Z".into(),
            observed_until: "2020-01-01T00:00:01Z".into(),
            records: 1,
            sources: snapshot().sources,
        }),
    }
}

#[test]
fn client_acquisition_clocks_never_supply_native_origin_or_bound_native_property_times() {
    let input = snapshot();
    assert!(input.is_valid());
    assert!(input.context.native().is_none());
    let encoded = serde_json::to_vec(&input).unwrap();
    let decoded: ReplicationSnapshot = serde_json::from_slice(&encoded).unwrap();
    assert_eq!(input, decoded);
    let ReplicationFact::Native(point) = &decoded.observations[0].fact else { panic!() };
    assert_eq!(point.measurements.properties["inactive_since"], "2026-11-16T12:00:00Z");
    assert_eq!(point.measurements.counters[0].value, MeasurementValue::Integer("9007199254740993".into()));
    assert!(
        ReplicationReadContext::collector(SystemTime::UNIX_EPOCH + Duration::from_secs(1), SystemTime::UNIX_EPOCH)
            .is_err()
    );
}

#[test]
fn original_native_scope_and_counts_survive_readback_without_manufacturing_a_member() {
    let source = source();
    let input = ReplicationSnapshot::from_source(&source, snapshot().observations).unwrap();
    assert!(input.is_valid());
    let context = input.context.native().unwrap();
    assert_eq!(context.target, source.target);
    assert_eq!(context.partition, SourcePartition::Target);
    assert!(!context.has_qualified_local_owner());
    assert_eq!(input.sources[0].state, SourceState::Partial);
    let mut missing = ReplicationSnapshot::from_source(&source, vec![]).unwrap();
    assert!(missing.is_valid_read(false));
    assert!(!missing.is_valid());
    assert_eq!(missing.sources[0].records, 1);
    missing.sources[0].state = SourceState::Empty;
    assert!(!missing.is_valid_read(false));
}

#[test]
fn positive_rows_cannot_borrow_another_source_empty_header_or_duplicate_identity() {
    let mut input = snapshot();
    input.observations.push(input.observations[0].clone());
    assert!(!input.is_valid_read(false));
    input.observations.pop();
    input.sources.push(input.sources[0].clone());
    assert!(!input.is_valid_read(false));
    input.sources.pop();
    input.observations[0].source = "unexpected".into();
    assert!(!input.is_valid_read(false));
    input.observations[0].source = "pg_replication_slots".into();
    input.sources[0] =
        StateSourceCoverage::unavailable("pg_replication_slots".into(), ErrorCode::PermissionDenied, None);
    assert!(!input.is_valid_read(false));
    input.observations.clear();
    assert!(input.is_valid());
    assert_eq!(input.sources[0].state, SourceState::Unavailable);
}

#[test]
fn native_descriptor_contradictions_fail_before_a_report_can_be_built() {
    let mut source = source();
    let SourceObservation::Snapshot(frame) = &mut source.observation else { panic!() };
    frame.dataset = SnapshotDataset::Operational;
    assert!(ReplicationSnapshot::from_source(&source, snapshot().observations).is_err());
    let mut source = self::source();
    let SourceObservation::Snapshot(frame) = &mut source.observation else { panic!() };
    frame.sources[0].complete_scope = true;
    assert!(ReplicationSnapshot::from_source(&source, snapshot().observations).is_err());
}
