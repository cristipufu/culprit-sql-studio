use super::*;
use crate::{Snapshot, SourceOutcome, SourceState};

fn read() -> Box<WaitCounterSnapshot> {
    let row = WaitCounterObservation {
        source: "private-source-canary".into(),
        wait_type: "private-wait-canary".into(),
        mechanism: None,
        scope: crate::MeasurementScope::Server,
        reset: ResetMarker::Generation("private-reset-canary".into()),
        waiting_tasks_count: "9007199254740993".into(),
        wait_time_ms: "9007199254740994".into(),
        signal_wait_time_ms: "1".into(),
        max_wait_time_ms: "7".into(),
    };
    Box::new(WaitCounterSnapshot {
        context: Some(ObservationReadContext::Collector {
            observed_from: "2020-01-01T00:00:00.000000001Z".into(),
            observed_until: "2020-01-01T00:00:00.000000002Z".into(),
        }),
        sources: vec![StateSourceCoverage::collected(row.source.clone(), 1)],
        observations: vec![row],
        read_complete: true,
    })
}

#[test]
fn sharing_preserves_exact_counter_semantics_counts_and_partial_body_while_masking_private_labels() {
    let mut original = read();
    let row = WaitCounterObservation {
        source: crate::source_id::WAITS.into(),
        wait_type: "RBIO_RG_STORAGE".into(),
        mechanism: Engine::Mssql.spec().native_wait_mechanism("RBIO_RG_STORAGE"),
        ..original.observations[0].clone()
    };
    original.sources.push(StateSourceCoverage::observed(row.source.clone(), 2, false));
    original.observations.push(row);
    original.read_complete = false;
    let before = original.observations.iter().map(WaitCounterObservation::resource_wait_time_ms).collect::<Vec<_>>();
    let shared = redact(original, Engine::Mssql).unwrap();
    assert!(shared.is_valid_for(Engine::Mssql));
    assert_eq!(
        shared.observations.iter().map(WaitCounterObservation::resource_wait_time_ms).collect::<Vec<_>>(),
        before
    );
    assert_eq!(shared.observations[1].wait_type, "RBIO_RG_STORAGE");
    assert_eq!(shared.sources[1].records, 2);
    assert!(!shared.read_complete);
    let encoded = serde_json::to_string(&shared).unwrap();
    for canary in ["private-source-canary", "private-wait-canary", "private-reset-canary"] {
        assert!(!encoded.contains(canary));
    }
    assert!(encoded.contains("9007199254740993"));
    assert!(encoded.contains("2020-01-01T00:00:00.000000001Z"));
    assert_eq!(serde_json::to_string(&redact(shared, Engine::Mssql).unwrap()).unwrap(), encoded);
}

#[test]
fn invalid_originals_and_masking_collisions_are_withheld_with_a_stable_explicit_gap() {
    for case in ["invalid", "collision"] {
        let mut read = read();
        if case == "invalid" {
            read.observations[0].wait_type.clear();
        } else {
            let mut another = read.observations[0].clone();
            another.wait_type = opaque(&another.wait_type);
            read.observations.push(another);
            read.sources[0].records = 2;
            assert!(read.is_valid_for(Engine::Mssql));
        }
        let mut snapshot: Snapshot =
            serde_json::from_str(include_str!("../../../../../fixtures/mssql-demo.json")).unwrap();
        snapshot.pressure.get_or_insert_with(Default::default).wait_counters = Some(read);
        let shared = crate::redact::redact_snapshot(&snapshot);
        assert!(shared.pressure.as_ref().unwrap().wait_counters.is_none());
        let gap = shared.sources.iter().find(|source| source.source == crate::source_id::WAITS).unwrap();
        assert_eq!(gap.state, SourceState::Unavailable);
        assert_eq!(gap.reason_message, Some(crate::message!("view.wait_counters.invalid")));
        let encoded = serde_json::to_string(&shared).unwrap();
        assert!(!encoded.contains("private-source-canary"));
        assert!(!encoded.contains("private-reset-canary"));
        assert_eq!(
            serde_json::to_value(crate::redact::redact_snapshot(&shared)).unwrap(),
            serde_json::to_value(&shared).unwrap()
        );
    }
}

#[test]
fn source_outcomes_and_native_context_use_existing_snapshot_privacy_policy() {
    let mut snapshot: Snapshot = serde_json::from_str(include_str!("../../../../../fixtures/mssql-demo.json")).unwrap();
    let mut read = read();
    let source = CollectionDescription {
        target: crate::DatabaseIdentity::server(
            Engine::Mssql,
            "db",
            "private-host-canary",
            1433,
            Some("private-incarnation-canary".into()),
        )
        .into(),
        partition: SourcePartition::Target,
        source: "logical-source".into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::Snapshot(StateSnapshotDescription {
            dataset: SnapshotDataset::WaitCounters,
            observed_from: "2020-01-01T00:00:00Z".into(),
            observed_until: "2020-01-01T00:00:01Z".into(),
            records: 1,
            sources: read.sources.clone(),
        }),
    };
    read.context =
        Some(ObservationReadContext::Native { context: Box::new(NativeReadContext::from_snapshot(&source).unwrap()) });
    snapshot.sources.push(SourceOutcome {
        source: "private-source-canary".into(),
        state: SourceState::Collected,
        reason: None,
        reason_message: None,
    });
    snapshot.pressure.get_or_insert_with(Default::default).wait_counters = Some(read);
    let shared = crate::redact::redact_snapshot(&snapshot);
    let read = shared.pressure.as_ref().unwrap().wait_counters.as_ref().unwrap();
    assert!(read.is_valid_for(Engine::Mssql));
    assert!(shared.sources.iter().any(|source| source.source == read.sources[0].source));
    let encoded = serde_json::to_string(&shared).unwrap();
    assert!(!encoded.contains("private-host-canary"));
    assert!(!encoded.contains("private-incarnation-canary"));
    assert!(!encoded.contains("private-source-canary"));
}
