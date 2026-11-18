use super::*;

fn component_read() -> CapturedOperationalRead {
    let point: HyperscaleLogComponent =
        serde_json::from_str(include_str!("../../hyperscale_log_components/tests/component.json")).unwrap();
    let mut database = crate::DatabaseIdentity::server(Engine::Mssql, "app", "fixture.invalid", 1433, None);
    database.incarnation = Some(point.database_incarnation.clone());
    let source = CollectionDescription {
        target: database.into(),
        partition: SourcePartition::Target,
        source: HYPERSCALE_LOG_COMPONENTS_SOURCE.into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::Snapshot(StateSnapshotDescription {
            dataset: SnapshotDataset::HyperscaleLogComponents,
            observed_from: "2026-11-18T12:00:00.000000001Z".into(),
            observed_until: "2026-11-18T12:00:00.000000009Z".into(),
            records: 1,
            sources: vec![StateSourceCoverage::observed(HYPERSCALE_LOG_COMPONENTS_SOURCE.into(), 1, false)],
        }),
    };
    let row = OperationalObservation {
        source: HYPERSCALE_LOG_COMPONENTS_SOURCE.into(),
        key: point.key().unwrap(),
        fact: OperationalFact::HyperscaleLogComponent(Box::new(point)),
    };
    bind(source, vec![row], true)
}

#[test]
fn component_capture_retains_original_positions_namespace_and_native_unknowns() {
    let original = component_read();
    assert!(original.is_valid());
    assert_eq!(original.source_coverage(HYPERSCALE_LOG_COMPONENTS_SOURCE).unwrap().state, SourceState::Partial);
    assert!(original.read_complete);
    assert_eq!(original.context().unwrap().observed_until, "2026-11-18T12:00:00.000000009Z");
    let references = original.captured_references().unwrap();
    assert_eq!(references.len(), 1);
    assert_eq!(references[0].row_ordinal, 0);
    references[0]
        .validate_canonical_record(
            &original.description,
            &TelemetryRecord::Operational(original.observations[0].clone()),
        )
        .unwrap();
    let copy: CapturedOperationalRead = serde_json::from_slice(&serde_json::to_vec(&original).unwrap()).unwrap();
    assert_eq!(copy, original);
    let mut omitted = original.clone();
    omitted.observations.clear();
    omitted.origin.positions.clear();
    assert!(!omitted.is_valid());
    omitted.read_complete = false;
    assert!(omitted.is_valid());
    assert_eq!(omitted.source_coverage(HYPERSCALE_LOG_COMPONENTS_SOURCE).unwrap().records, 1);
    assert!(omitted.memory_clerk_source().is_err());
}

#[test]
fn component_scope_role_clock_body_and_original_position_are_never_repaired() {
    for change in ["namespace", "epoch", "clock", "native-clock", "role", "body", "ordinal", "digest", "dataset"] {
        let mut changed = component_read();
        match change {
            "namespace" => {
                let CollectionTarget::Database(database) = &mut changed.description.target else { panic!() };
                database.incarnation = Some("foreign-native-database".into());
            }
            "epoch" => {
                changed.description.partition = SourcePartition::InstanceLocal { member: None, epoch: None };
            }
            "clock" => {
                let SourceObservation::Snapshot(header) = &mut changed.description.observation else { panic!() };
                header.observed_until = "2026-11-18T12:00:00.000000010Z".into();
            }
            "native-clock" => {
                let OperationalFact::HyperscaleLogComponent(point) = &mut changed.observations[0].fact else {
                    panic!()
                };
                point.last_reported_time_without_offset = None;
            }
            "role" => {
                let OperationalFact::HyperscaleLogComponent(point) = &mut changed.observations[0].fact else {
                    panic!()
                };
                point.role = 255;
            }
            "body" => {
                let OperationalFact::HyperscaleLogComponent(point) = &mut changed.observations[0].fact else {
                    panic!()
                };
                point.catchup_distance_bytes = "1".into();
            }
            "ordinal" => changed.origin.positions[0].row_ordinal = 1,
            "digest" => changed.origin.positions[0].payload_sha256 = digest(b"replacement"),
            "dataset" => {
                let SourceObservation::Snapshot(header) = &mut changed.description.observation else { panic!() };
                header.dataset = SnapshotDataset::Settings;
            }
            _ => unreachable!(),
        }
        assert!(!changed.is_valid(), "{change}");
        assert!(changed.captured_references().is_err(), "{change}");
    }
}
