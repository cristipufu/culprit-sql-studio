use super::*;

fn captured() -> CapturedOperationalRead {
    let resource = SharedResourceIdentity {
        engine: Engine::Mssql,
        kind: SharedResourceKind::Instance,
        namespace: NativeIdentity::new("original-private-clerk-namespace"),
        native: NativeIdentity::new("original-private-clerk-instance"),
    };
    let mut unrelated = sample(2);
    unrelated.identity = NativeIdentity::new("unrelated-original-group");
    let mut other = observation(unrelated);
    other.source = "unrelated original native source".into();
    let observations = vec![observation(sample(0)), observation(sample(1)), other];
    let description = CollectionDescription {
        target: CollectionTarget::Shared(resource.clone()),
        partition: SourcePartition::InstanceLocal {
            member: Some(resource),
            epoch: Some("original-native-process".into()),
        },
        source: "original-operational-reader".into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::Snapshot(StateSnapshotDescription {
            dataset: SnapshotDataset::Operational,
            observed_from: context().observed_from,
            observed_until: context().observed_until,
            records: 3,
            sources: vec![
                StateSourceCoverage::collected(culprit_core::source_id::MEMORY_CLERKS.into(), 2),
                StateSourceCoverage::collected("unrelated original native source".into(), 1),
            ],
        }),
    };
    let identity = CaptureIdentity {
        observation_id: "c".repeat(32),
        revision: Some(CaptureRevision { outbox_id: "d".repeat(32), owner_epoch: "1".into(), sequence: "1".into() }),
    };
    let capture = DiagnosticCaptureRef {
        capture: identity.clone(),
        manifest_sha256: digest(b"original-clerk-capture-manifest"),
        source_scope_id: description.captured_range().unwrap().scope_id,
    };
    let positions = observations
        .iter()
        .enumerate()
        .map(|(ordinal, row)| {
            let row = ExportedRecord::new(&description, &identity, TelemetryRecord::Operational(row.clone())).unwrap();
            CapturedSnapshotPosition::from_export(&row, ordinal as u32).unwrap()
        })
        .collect();
    CapturedOperationalRead::captured(description, capture, positions, observations, true).unwrap()
}

#[test]
fn original_shared_capture_reanalysis_preserves_scope_clocks_and_unrelated_rows() {
    let raw = captured();
    let original_bytes = serde_json::to_vec(&raw).unwrap();
    let mut snapshot = snapshot();
    snapshot.pressure.get_or_insert_with(Default::default).resource_counters = None;
    snapshot.operations.memory_clerks = Some(Box::new(raw.clone()));
    let decoded: Snapshot = serde_json::from_slice(&serde_json::to_vec(&snapshot).unwrap()).unwrap();
    let report = explain_snapshot(&decoded).unwrap().unwrap();
    assert_eq!(report.context.as_ref().unwrap().native().unwrap().target, raw.description.target);
    let original_context = raw.context().unwrap();
    assert_eq!(
        report.context.as_ref().unwrap().bounds(),
        (original_context.observed_from.as_str(), original_context.observed_until.as_str())
    );
    assert_eq!(report.native_rows, Some(2));
    assert_eq!(report.observed_rows, 2);
    assert_eq!(report.read_complete, Some(true));
    assert_eq!(field(&report.entries[0], "pages_kb").value.as_deref(), Some("9007199254740993"));
    let retained = decoded.operations.memory_clerks.as_ref().unwrap();
    assert_eq!(retained.description.observation.snapshot().unwrap().records, 3);
    assert_eq!(retained.observations.len(), 3);
    assert_eq!(retained.origin.positions[2].row_ordinal, 2);
    assert_eq!(serde_json::to_vec(retained.as_ref()).unwrap(), original_bytes);
    assert!(decoded.pressure.as_ref().unwrap().resource_counters.is_none());
}

#[test]
fn missing_original_clerk_bodies_retain_native_counts_and_other_source_positions() {
    let mut raw = captured();
    raw.observations.drain(..2);
    raw.origin.positions.drain(..2);
    raw.read_complete = false;
    assert!(raw.is_valid());
    let report = explain_captured(&raw).unwrap();
    assert_eq!(report.source_state, Some(SourceState::Partial));
    assert_eq!(report.native_source.as_ref().unwrap().state, SourceState::Collected);
    assert_eq!((report.native_rows, report.observed_rows), (Some(2), 0));
    assert_eq!(raw.origin.positions[0].row_ordinal, 2);
    assert_eq!(raw.observations[0].source, "unrelated original native source");
    assert!(report.entries.is_empty());
}

#[test]
fn captured_and_live_clerk_populations_conflict_instead_of_being_pooled_or_overwritten() {
    let mut snapshot = snapshot();
    snapshot.operations.memory_clerks = Some(Box::new(captured()));
    snapshot.pressure.get_or_insert_with(Default::default).resource_counters = Some(vec![sample(8)]);
    assert!(explain_snapshot(&snapshot).is_err());
    let report = crate::analyze(snapshot);
    assert!(report.memory_clerks.is_none());
    assert!(
        report.snapshot.sources.iter().any(|source| source.source == culprit_core::source_id::MEMORY_CLERKS
            && source.state == SourceState::Unavailable)
    );
    assert_eq!(report.snapshot.operations.memory_clerks.as_ref().unwrap().observations.len(), 3);
    assert_eq!(report.snapshot.pressure.as_ref().unwrap().resource_counters.as_ref().unwrap().len(), 1);
}

#[test]
fn changed_original_bytes_or_a_database_target_do_not_gain_captured_clerk_authority() {
    let mut raw = captured();
    let OperationalFact::CounterSet(group) = &mut raw.observations[0].fact else { panic!() };
    group.counters[1].value = MeasurementValue::Integer("9007199254740994".into());
    assert!(raw.observations[0].is_valid());
    assert!(explain_captured(&raw).is_err());
    let original = captured();
    let mut description = original.description;
    description.target = context().target;
    description.partition = SourcePartition::Target;
    let identity = original.origin.capture.capture.clone();
    let capture = DiagnosticCaptureRef {
        source_scope_id: description.captured_range().unwrap().scope_id,
        ..original.origin.capture
    };
    let positions = original
        .observations
        .iter()
        .enumerate()
        .map(|(ordinal, row)| {
            let row = ExportedRecord::new(&description, &identity, TelemetryRecord::Operational(row.clone())).unwrap();
            CapturedSnapshotPosition::from_export(&row, ordinal as u32).unwrap()
        })
        .collect();
    let database_read =
        CapturedOperationalRead::captured(description, capture, positions, original.observations, true).unwrap();
    assert!(database_read.is_valid());
    assert!(explain_captured(&database_read).is_err());
}

#[test]
fn sharing_original_capture_keeps_a_repeatable_unavailable_view_without_proof_inputs() {
    let mut snapshot = snapshot();
    snapshot.pressure.get_or_insert_with(Default::default).resource_counters = None;
    snapshot.operations.memory_clerks = Some(Box::new(captured()));
    let before = serde_json::to_vec(&snapshot.operations.memory_clerks).unwrap();
    let shared = culprit_core::redact::redact_snapshot(&snapshot);
    let report = explain_snapshot(&shared).unwrap().unwrap();
    assert_eq!(report.source_state, Some(SourceState::Unavailable));
    assert!(report.entries.is_empty());
    assert!(report.native_rows.is_none());
    assert!(report.context.is_none());
    let repeated = culprit_core::redact::redact_snapshot(&shared);
    assert_eq!(explain_snapshot(&repeated).unwrap().unwrap().source_state, Some(SourceState::Unavailable));
    assert_eq!(before, serde_json::to_vec(&snapshot.operations.memory_clerks).unwrap());
}
