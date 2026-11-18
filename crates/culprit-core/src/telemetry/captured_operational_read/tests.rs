//! Controlled native facts test structural retention; the fixture issues no archive authentication.
use super::*;
use crate::{Engine, MeasurementScope, SourceState};

fn read() -> CapturedOperationalRead {
    let member = SharedResourceIdentity {
        engine: Engine::Mssql,
        kind: SharedResourceKind::Instance,
        namespace: NativeIdentity::new("private-native-namespace"),
        native: NativeIdentity::new("private-instance-owner"),
    };
    let description = CollectionDescription {
        target: CollectionTarget::Shared(member.clone()),
        partition: SourcePartition::InstanceLocal {
            member: Some(member),
            epoch: Some("original-private-process-epoch".into()),
        },
        source: "original-private-operational-reader".into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::Snapshot(StateSnapshotDescription {
            dataset: SnapshotDataset::Operational,
            observed_from: "2026-11-18T12:00:00.000000001Z".into(),
            observed_until: "2026-11-18T12:00:00.000000009Z".into(),
            records: 3,
            sources: vec![
                StateSourceCoverage::collected(crate::source_id::MEMORY_CLERKS.into(), 2),
                StateSourceCoverage::collected("unrelated-private-native-source".into(), 1),
            ],
        }),
    };
    let observations = (0..3)
        .map(|index| {
            let source = if index < 2 { crate::source_id::MEMORY_CLERKS } else { "unrelated-private-native-source" };
            let sample = NativeCounterSet {
                identity: NativeIdentity::new(format!("private-native-group-{index}")),
                scope: MeasurementScope::Server,
                counters: vec![NativeCounterSample::gauge(
                    "exact-native-count",
                    9_007_199_254_740_993,
                    CounterUnit::Count,
                    MeasurementScope::Server,
                    "private-native-object",
                    "private-native-column",
                )],
                ratios: vec![],
                ratio_gaps: vec![],
                properties: Default::default(),
            };
            OperationalObservation {
                source: source.into(),
                key: sample.key().unwrap(),
                fact: OperationalFact::CounterSet(Box::new(sample)),
            }
        })
        .collect::<Vec<_>>();
    bind(description, observations, true)
}
fn bind(
    description: CollectionDescription,
    observations: Vec<OperationalObservation>,
    complete: bool,
) -> CapturedOperationalRead {
    let identity = CaptureIdentity {
        observation_id: "a".repeat(32),
        revision: Some(CaptureRevision { outbox_id: "b".repeat(32), owner_epoch: "3".into(), sequence: "4".into() }),
    };
    let capture = DiagnosticCaptureRef {
        capture: identity.clone(),
        manifest_sha256: digest(b"original-immutable-manifest"),
        source_scope_id: description.captured_range().unwrap().scope_id,
    };
    let positions = observations
        .iter()
        .enumerate()
        .map(|(ordinal, row)| {
            let exported =
                ExportedRecord::new(&description, &identity, TelemetryRecord::Operational(row.clone())).unwrap();
            CapturedSnapshotPosition::from_export(&exported, ordinal as u32).unwrap()
        })
        .collect();
    CapturedOperationalRead::captured(description, capture, positions, observations, complete).unwrap()
}

#[test]
fn all_original_sources_positions_and_exact_values_survive_roundtrip_without_native_read_identity() {
    let original = read();
    assert!(original.is_valid());
    let bytes = serde_json::to_vec(&original).unwrap();
    let restored: CapturedOperationalRead = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(restored, original);
    assert_eq!(restored.observations.len(), 3);
    assert_eq!(
        restored.origin.positions.iter().map(|position| position.row_ordinal).collect::<Vec<_>>(),
        vec![0, 1, 2]
    );
    let refs = restored.captured_references().unwrap();
    assert_eq!(refs.len(), 3);
    assert!(refs.iter().all(|reference| reference.capture_ref() == original.origin.capture));
    let context = restored.context().unwrap();
    assert_eq!(context.target, original.description.target);
    assert_eq!(context.partition, original.description.partition);
    assert_eq!(context.observed_from, "2026-11-18T12:00:00.000000001Z");
    assert_eq!(restored.source_coverage(crate::source_id::MEMORY_CLERKS).unwrap().records, 2);
    assert!(restored.memory_clerk_source().is_ok());
    assert!(!String::from_utf8(bytes).unwrap().contains("native-read"));
    let OperationalFact::CounterSet(sample) = &restored.observations[0].fact else { panic!() };
    assert_eq!(sample.counters[0].value.integer(), Some(9_007_199_254_740_993));
}

#[test]
fn a_filtered_body_cannot_claim_complete_and_a_partial_body_preserves_original_ordinals_and_counts() {
    let mut partial = read();
    partial.observations.remove(1);
    partial.origin.positions.remove(1);
    assert!(!partial.is_valid());
    partial.read_complete = false;
    assert!(partial.is_valid());
    assert_eq!(partial.description.observation.snapshot().unwrap().records, 3);
    assert_eq!(partial.source_coverage(crate::source_id::MEMORY_CLERKS).unwrap().records, 2);
    assert_eq!(
        partial.captured_references().unwrap().iter().map(|row| row.row_ordinal).collect::<Vec<_>>(),
        vec![0, 2]
    );
    assert!(partial.memory_clerk_source().is_ok());
}

#[test]
fn changed_descriptor_or_proof_cannot_rebind_original_rows() {
    for field in [
        "target",
        "member",
        "epoch",
        "clock",
        "coverage",
        "source",
        "manifest",
        "capture",
        "source_digest",
        "ordinal",
        "position_digest",
    ] {
        let mut changed = read();
        match field {
            "target" => {
                let CollectionTarget::Shared(resource) = &mut changed.description.target else { panic!() };
                resource.native.id = "different-owner".into();
            }
            "member" => {
                let SourcePartition::InstanceLocal { member: Some(member), .. } = &mut changed.description.partition
                else {
                    panic!()
                };
                member.native.id = "different-member".into();
            }
            "epoch" => {
                let SourcePartition::InstanceLocal { epoch, .. } = &mut changed.description.partition else { panic!() };
                *epoch = Some("changed-process".into());
            }
            "clock" => {
                let SourceObservation::Snapshot(header) = &mut changed.description.observation else { panic!() };
                header.observed_until = "2026-11-18T12:00:00.000000010Z".into();
            }
            "coverage" => {
                let SourceObservation::Snapshot(header) = &mut changed.description.observation else { panic!() };
                header.sources[0] = StateSourceCoverage::observed(crate::source_id::MEMORY_CLERKS.into(), 2, false);
            }
            "source" => changed.description.source = "replacement-reader".into(),
            "manifest" => changed.origin.capture.manifest_sha256 = "invalid-manifest-digest".into(),
            "capture" => changed.origin.capture.capture.revision.as_mut().unwrap().sequence = "5".into(),
            "source_digest" => changed.origin.source_sha256 = digest(b"other-descriptor"),
            "ordinal" => changed.origin.positions[0].row_ordinal = 3,
            "position_digest" => changed.origin.positions[0].payload_sha256 = digest(b"other-original-row"),
            _ => unreachable!(),
        }
        assert!(!changed.is_valid(), "{field}");
        assert!(changed.captured_references().is_err(), "{field}");
    }
}

#[test]
fn undeclared_duplicate_or_changed_native_rows_are_refused_without_repair() {
    for field in ["source", "key", "body", "duplicate-row", "duplicate-position", "missing-position"] {
        let mut changed = read();
        match field {
            "source" => changed.observations[0].source = "undeclared-source".into(),
            "key" => changed.observations[0].key = digest(b"another-row"),
            "body" => {
                let OperationalFact::CounterSet(sample) = &mut changed.observations[0].fact else { panic!() };
                sample.counters[0].value = MeasurementValue::Integer("9007199254740994".into());
            }
            "duplicate-row" => changed.observations[1] = changed.observations[0].clone(),
            "duplicate-position" => changed.origin.positions[1].row_ordinal = 0,
            "missing-position" => {
                changed.origin.positions.pop();
            }
            _ => unreachable!(),
        }
        assert!(!changed.is_valid(), "{field}");
        assert!(changed.captured_references().is_err(), "{field}");
    }
}

#[test]
fn native_partial_unavailable_and_unknown_member_are_independent_from_body_completion() {
    let mut original = read();
    let SourceObservation::Snapshot(header) = &mut original.description.observation else { panic!() };
    header.sources[0] = StateSourceCoverage::observed(crate::source_id::MEMORY_CLERKS.into(), 2, false);
    let partial = bind(original.description, original.observations, true);
    assert_eq!(partial.memory_clerk_source().unwrap().state, SourceState::Partial);
    assert!(partial.read_complete);
    let mut description = partial.description;
    let SourceObservation::Snapshot(header) = &mut description.observation else { panic!() };
    header.sources[0] =
        StateSourceCoverage::unavailable(crate::source_id::MEMORY_CLERKS.into(), ErrorCode::PermissionDenied, None);
    header.records = 1;
    description.partition = SourcePartition::InstanceLocal { member: None, epoch: None };
    assert!(
        !description.partition_is_consistent(),
        "an unidentified member cannot preserve complete coverage for another declared native source"
    );
    assert!(description.captured_range().is_err());
    let SourceObservation::Snapshot(header) = &mut description.observation else { panic!() };
    header.sources[1] = StateSourceCoverage::observed(header.sources[1].source.clone(), 1, false);
    let unavailable = bind(description, vec![partial.observations[2].clone()], true);
    assert_eq!(unavailable.memory_clerk_source().unwrap().state, SourceState::Unavailable);
    assert!(!unavailable.context().unwrap().has_qualified_local_owner());
    assert!(unavailable.read_complete, "all original selected bodies remain retained despite partial native scope");
    let header = unavailable.description.observation.snapshot().unwrap();
    assert_eq!(header.sources[1].state, SourceState::Partial);
    assert!(!header.sources[1].complete_scope);
    assert_eq!(header.sources[1].records, 1);
    assert_eq!(header.records, 1);
}

#[test]
fn generic_operational_capture_is_not_a_clerk_read_when_scope_dataset_or_family_differs() {
    for change in ["dataset", "database", "no-clerk-source"] {
        let original = read();
        let mut description = original.description;
        let mut observations = original.observations;
        match change {
            "dataset" => {
                let SourceObservation::Snapshot(header) = &mut description.observation else { panic!() };
                header.dataset = SnapshotDataset::Settings;
            }
            "database" => {
                description.target =
                    crate::DatabaseIdentity::server(Engine::Mssql, "app", "reader.invalid", 1433, None).into();
                description.partition = SourcePartition::Target;
            }
            "no-clerk-source" => {
                let SourceObservation::Snapshot(header) = &mut description.observation else { panic!() };
                header.sources.remove(0);
                header.records = 1;
                observations.drain(..2);
            }
            _ => unreachable!(),
        }
        if change == "dataset" {
            let origin =
                CapturedSnapshotOrigin::captured(&description, original.origin.capture, original.origin.positions)
                    .unwrap();
            assert!(!CapturedOperationalRead { origin, description, observations, read_complete: true }.is_valid());
        } else {
            let other = bind(description, observations, true);
            assert!(other.is_valid());
            assert!(other.memory_clerk_source().is_err());
        }
    }
}

#[test]
fn sharing_drops_the_whole_original_carrier_and_retains_only_independent_live_counters() {
    let mut snapshot: crate::Snapshot =
        serde_json::from_str(include_str!("../../../../../fixtures/mssql-demo.json")).unwrap();
    let live = NativeCounterSet {
        identity: NativeIdentity::new("independent-live-counter"),
        scope: MeasurementScope::Server,
        counters: vec![NativeCounterSample::gauge(
            "live-count",
            17,
            CounterUnit::Count,
            MeasurementScope::Server,
            "other live object",
            "other live column",
        )],
        ratios: vec![],
        ratio_gaps: vec![],
        properties: Default::default(),
    };
    snapshot.pressure.get_or_insert_with(Default::default).resource_counters = Some(vec![live]);
    snapshot.operations.memory_clerks = Some(Box::new(read()));
    let before = serde_json::to_vec(&snapshot.operations.memory_clerks).unwrap();
    let shared = crate::redact::redact_snapshot(&snapshot);
    assert!(shared.operations.memory_clerks.is_none());
    let live = shared.pressure.as_ref().unwrap().resource_counters.as_ref().unwrap();
    assert_eq!(live.len(), 1);
    assert_eq!(live[0].counters[0].value.integer(), Some(17));
    let text = serde_json::to_string(&shared).unwrap();
    for private in [
        "original-private",
        "private-native-group",
        "unrelated-private-native-source",
        "private-instance-owner",
        "source_sha256",
        "payload_sha256",
    ] {
        assert!(!text.contains(private), "{private}");
    }
    let source = shared.sources.iter().find(|source| source.source == crate::source_id::MEMORY_CLERKS).unwrap();
    assert_eq!(source.state, SourceState::Unavailable);
    let repeated = crate::redact::redact_snapshot(&shared);
    assert!(repeated.operations.memory_clerks.is_none());
    assert_eq!(
        repeated.sources.iter().find(|source| source.source == crate::source_id::MEMORY_CLERKS).unwrap().reason_message,
        source.reason_message
    );
    assert_eq!(before, serde_json::to_vec(&snapshot.operations.memory_clerks).unwrap());
}

#[test]
fn counters_only_projection_never_includes_captured_proof_or_rows() {
    let operations =
        crate::operations::OperationalStats { memory_clerks: Some(Box::new(read())), ..Default::default() };
    let encoded = serde_json::to_string(&operations.counters_only()).unwrap();
    for private in ["memory_clerks", "origin", "source_sha256", "original-private", "private-native-group"] {
        assert!(!encoded.contains(private), "{private}");
    }
}

#[test]
fn a_numeric_equality_cannot_substitute_different_bytes_in_an_unrelated_original_row() {
    let mut original = read();
    let OperationalFact::CounterSet(sample) = &mut original.observations[2].fact else { panic!() };
    sample.counters[0].value = MeasurementValue::Real(0.0);
    let original = bind(original.description, original.observations, true);
    let mut changed = original.clone();
    let OperationalFact::CounterSet(sample) = &mut changed.observations[2].fact else { panic!() };
    sample.counters[0].value = MeasurementValue::Real(-0.0);
    assert_eq!(changed.observations, original.observations);
    assert!(changed.observations[2].is_valid());
    assert!(!changed.is_valid());
    assert!(changed.captured_references().is_err());
}

#[test]
fn captured_origin_has_no_native_read_fallback_or_unknown_proof_fields() {
    let original = serde_json::to_value(read()).unwrap();
    for field in ["read_id", "kind", "unknown_proof"] {
        let mut changed = original.clone();
        changed["origin"][field] = serde_json::Value::String(digest(b"invented-native-read"));
        assert!(serde_json::from_value::<CapturedOperationalRead>(changed).is_err(), "{field}");
    }
    let mut changed = original;
    changed["origin"]["positions"][0]["stream"] = serde_json::Value::String("activity-observations".into());
    assert!(serde_json::from_value::<CapturedOperationalRead>(changed).is_err());
}
