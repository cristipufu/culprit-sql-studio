use super::*;
fn point() -> HyperscaleLogComponent {
    serde_json::from_str(include_str!("tests/component.json")).unwrap()
}
fn source() -> CollectionDescription {
    let mut database = DatabaseIdentity::server(crate::Engine::Mssql, "app", "fixture.invalid", 1433, None);
    database.incarnation = Some(point().database_incarnation);
    CollectionDescription {
        target: database.into(),
        partition: SourcePartition::Target,
        source: HYPERSCALE_LOG_COMPONENTS_SOURCE.into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::Snapshot(StateSnapshotDescription {
            dataset: SnapshotDataset::HyperscaleLogComponents,
            observed_from: "2026-11-18T12:00:00Z".into(),
            observed_until: "2026-11-18T12:00:01Z".into(),
            records: 1,
            sources: vec![StateSourceCoverage::observed(HYPERSCALE_LOG_COMPONENTS_SOURCE.into(), 1, false)],
        }),
    }
}
fn row(point: HyperscaleLogComponent) -> OperationalObservation {
    OperationalObservation {
        source: HYPERSCALE_LOG_COMPONENTS_SOURCE.into(),
        key: point.key().unwrap(),
        fact: OperationalFact::HyperscaleLogComponent(Box::new(point)),
    }
}
#[test]
fn exact_signed_native_values_and_independent_offset_free_report_clock_survive_roundtrip() {
    let original = point();
    assert!(original.is_valid());
    let restored: HyperscaleLogComponent = serde_json::from_slice(&serde_json::to_vec(&original).unwrap()).unwrap();
    assert_eq!(restored, original);
    for value in ["-1", "01", "+1", "1.0", "9223372036854775808"] {
        let mut changed = point();
        changed.catchup_distance_bytes = value.into();
        assert!(!changed.is_valid());
    }
    let mut changed = point();
    changed.last_reported_time_without_offset = Some("2026-11-18T12:00:00.003Z".into());
    assert!(!changed.is_valid());
    changed.last_reported_time_without_offset = None;
    changed.replica_id = None;
    changed.file_id = Some("1".into());
    changed.role = 255;
    changed.role_desc = None;
    assert!(changed.is_valid());
}
#[test]
fn source_namespace_and_original_receipt_cannot_be_repaired_or_reassigned() {
    let source = source();
    let original = TelemetryRecord::Operational(row(point()));
    let exported = ExportedRecord::new(
        &source,
        &CaptureIdentity {
            observation_id: "native-hs-components".into(),
            revision: Some(CaptureRevision {
                outbox_id: "owned-components-outbox".into(),
                owner_epoch: "1".into(),
                sequence: "1".into(),
            }),
        },
        original,
    )
    .unwrap();
    let reference = DiagnosticEvidenceRef::from_export(&source, &digest(b"original manifest"), &exported, 0).unwrap();
    reference.validate_canonical_record(&source, &exported.record).unwrap();
    let mut changed = exported.record.clone();
    let TelemetryRecord::Operational(row) = &mut changed else { panic!() };
    let OperationalFact::HyperscaleLogComponent(changed_point) = &mut row.fact else { panic!() };
    changed_point.catchup_distance_bytes = "1".into();
    assert!(reference.validate_canonical_record(&source, &changed).is_err());
    let mut foreign = source.clone();
    let CollectionTarget::Database(database) = &mut foreign.target else { panic!() };
    database.incarnation = Some("foreign".into());
    assert!(!point().fits_source(&foreign));
}
#[test]
fn body_completion_native_partial_and_duplicate_component_identity_are_independent() {
    let source = source();
    let read = HyperscaleLogComponentRead::from_source(&source, vec![row(point())], true).unwrap();
    assert!(read.is_valid());
    let mut duplicate = read.clone();
    let SourceObservation::Snapshot(header) = &mut duplicate.description.observation else { panic!() };
    header.records = 2;
    header.sources[0].records = 2;
    let mut second = point();
    second.component_id = second.component_id.to_ascii_uppercase();
    second.role = 1;
    second.last_reported_time_without_offset = None;
    duplicate.observations.push(row(second));
    assert!(!duplicate.is_valid());
    assert!(HyperscaleLogComponentRead::from_source(&source, vec![], false).unwrap().is_valid());
    assert!(HyperscaleLogComponentRead::from_source(&source, vec![], true).is_err());
}
#[test]
fn snapshot_sharing_withholds_private_component_topology_and_keeps_the_explicit_gap() {
    let mut snapshot: crate::Snapshot =
        serde_json::from_str(include_str!("../../../../../fixtures/mssql-demo.json")).unwrap();
    let description = source();
    let observation = row(point());
    let identity = CaptureIdentity {
        observation_id: "original-hs-capture".into(),
        revision: Some(CaptureRevision {
            outbox_id: "original-hs-outbox".into(),
            owner_epoch: "1".into(),
            sequence: "1".into(),
        }),
    };
    let reference = DiagnosticCaptureRef {
        capture: identity.clone(),
        manifest_sha256: digest(b"original manifest"),
        source_scope_id: description.captured_range().unwrap().scope_id,
    };
    let exported =
        ExportedRecord::new(&description, &identity, TelemetryRecord::Operational(observation.clone())).unwrap();
    let position = CapturedSnapshotPosition::from_export(&exported, 0).unwrap();
    snapshot.operations.hyperscale_log_components = Some(Box::new(
        CapturedOperationalRead::captured(description, reference, vec![position], vec![observation], true).unwrap(),
    ));
    let shared = crate::redact::redact_snapshot(&snapshot);
    let text = serde_json::to_string(&shared).unwrap();
    assert!(shared.operations.hyperscale_log_components.is_none());
    assert!(!text.contains("11111111-2222-3333-4444-555555555555"));
    assert!(
        shared.sources.iter().any(|source| source.source == HYPERSCALE_LOG_COMPONENTS_SOURCE
            && source.state == crate::SourceState::Unavailable)
    );
    let again = crate::redact::redact_snapshot(&shared);
    assert_eq!(serde_json::to_value(shared).unwrap(), serde_json::to_value(again).unwrap());
}
