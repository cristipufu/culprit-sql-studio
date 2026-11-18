use super::*;

fn point() -> DatabasePropertiesObservation {
    serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../fixtures/database-properties.native.json"
    )))
    .unwrap()
}
fn source() -> CollectionDescription {
    CollectionDescription {
        target: crate::DatabaseIdentity::server(
            crate::Engine::Mssql,
            "sample",
            "fixture.invalid",
            1433,
            Some("logical-database".into()),
        )
        .into(),
        partition: SourcePartition::Target,
        source: "operational-state".into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::Snapshot(StateSnapshotDescription {
            dataset: SnapshotDataset::Operational,
            observed_from: "2020-01-01T12:00:00Z".into(),
            observed_until: "2020-01-01T12:00:01Z".into(),
            records: 1,
            sources: vec![StateSourceCoverage::collected(DATABASE_PROPERTIES_SOURCE.into(), 1)],
        }),
    }
}
fn read() -> DatabasePropertiesRead {
    let point = point();
    let row = OperationalObservation {
        source: DATABASE_PROPERTIES_SOURCE.into(),
        key: point.key().unwrap(),
        fact: OperationalFact::DatabaseProperties(Box::new(point)),
    };
    DatabasePropertiesRead::from_source(&source(), vec![row], true).unwrap()
}

#[test]
fn exact_native_flags_zero_and_original_scope_survive_serialization_without_a_current_member_claim() {
    let original = read();
    assert!(original.is_valid());
    let restored: DatabasePropertiesRead = serde_json::from_slice(&serde_json::to_vec(&original).unwrap()).unwrap();
    assert_eq!(restored, original);
    assert_eq!(restored.observation.as_ref().unwrap().target_recovery_time_seconds, Some(0));
    assert_eq!(restored.observation.as_ref().unwrap().catalog_read_only, Some(false));
    assert!(!restored.context.native().unwrap().has_qualified_local_owner());
}

#[test]
fn catalog_read_only_and_connected_updateability_are_independent_and_native_text_does_not_supply_codes() {
    let mut point = point();
    point.updateability = Some("READ_ONLY".into());
    assert!(point.is_valid());
    assert_eq!(point.catalog_read_only, Some(false));
    assert_eq!(point.writable(), Some(false));
    point.catalog_state.code = Some(255);
    assert_eq!(point.catalog_state.description.as_deref(), Some("ONLINE"));
    assert_eq!(point.state(DatabasePropertyCodeKind::CatalogState), None);
    point.snapshot_isolation.code = None;
    assert_eq!(point.state(DatabasePropertyCodeKind::SnapshotIsolation), None);
    point.updateability = Some("FUTURE_NATIVE_VALUE".into());
    assert_eq!(point.writable(), None);
}

#[test]
fn native_scope_clock_role_and_selected_source_count_conflicts_are_refused() {
    for change in ["foreign-db", "foreign-role", "outside-clock", "false-zero", "wrong-source", "empty"] {
        let mut read = read();
        match change {
            "foreign-db" => read.observation.as_mut().unwrap().database = "foreign".into(),
            "foreign-role" => {
                let ObservationReadContext::Native { context } = &mut read.context else { panic!() };
                context.database_writable = Some(false);
            }
            "outside-clock" => read.observation.as_mut().unwrap().observed_at = "2020-01-01T12:00:02Z".into(),
            "false-zero" => {
                read.observation = None;
            }
            "wrong-source" => read.source.source = "other".into(),
            "empty" => read.source = StateSourceCoverage::collected(DATABASE_PROPERTIES_SOURCE.into(), 0),
            _ => unreachable!(),
        }
        assert!(!read.is_valid(), "{change}");
    }
    let mut missing = read();
    missing.observation = None;
    missing.read_complete = false;
    assert!(missing.is_valid());
    assert_eq!(missing.source.records, 1);
}

#[test]
fn malformed_native_identity_finer_clock_and_negative_configuration_cannot_be_repaired_to_zero() {
    for change in ["id", "clock", "seconds", "name"] {
        let mut point = point();
        match change {
            "id" => point.native_database_id = "05".into(),
            "clock" => point.observed_at = "2020-01-01T12:00:00.0000000001Z".into(),
            "seconds" => point.target_recovery_time_seconds = Some(u32::MAX),
            "name" => point.database.clear(),
            _ => unreachable!(),
        }
        assert!(!point.is_valid(), "{change}");
        assert!(point.key().is_err());
    }
}
