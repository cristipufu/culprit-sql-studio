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

#[test]
fn last_checkdb_keeps_raw_local_datetime_and_distinguishes_the_native_no_record_sentinel_from_unknown() {
    let mut point = point();
    for (value, recorded) in [
        (None, None),
        (Some("1900-01-01T00:00:00"), Some(false)),
        (Some("1900-01-01T00:00:00.000"), Some(false)),
        (Some("1900-01-01T00:00:00.0"), Some(false)),
        (Some("2019-12-28T23:10:00.003"), Some(true)),
    ] {
        point.last_good_checkdb_without_offset = value.map(str::to_owned);
        assert!(point.is_valid());
        assert_eq!(point.has_recorded_checkdb(), recorded);
        let restored: DatabasePropertiesObservation =
            serde_json::from_value(serde_json::to_value(&point).unwrap()).unwrap();
        assert_eq!(restored.last_good_checkdb_without_offset.as_deref(), value);
    }
    for value in
        ["2020-01-01T12:00:00Z", "2020-01-01T12:00:00+02:00", "2020-02-30T12:00:00", "2020-01-01T12:00:00.0001"]
    {
        point.last_good_checkdb_without_offset = Some(value.into());
        assert!(!point.is_valid(), "{value}");
    }
}

#[test]
fn an_integrity_history_fact_cannot_be_borrowed_by_another_engine_or_a_missing_native_body() {
    for engine in crate::Engine::ALL {
        let mut read = read();
        read.context = ObservationReadContext::Collector {
            observed_from: "2020-01-01T12:00:00Z".into(),
            observed_until: "2020-01-01T12:00:01Z".into(),
        };
        let CollectionTarget::Database(database) = &mut read.target else { panic!() };
        database.engine = engine;
        assert_eq!(read.is_valid(), engine == crate::Engine::Mssql, "{engine:?}");
    }
    let mut missing = read();
    missing.observation = None;
    missing.read_complete = false;
    assert!(missing.is_valid());
    assert_eq!(missing.source.records, 1);
}

#[test]
fn auto_shrink_configuration_keeps_native_true_false_and_null_without_accepting_an_omitted_contract_field() {
    for native in [Some(true), Some(false), None] {
        let mut original = point();
        original.auto_shrink = native;
        assert!(original.is_valid());
        let bytes = serde_json::to_vec(&original).unwrap();
        let restored: DatabasePropertiesObservation = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(restored.auto_shrink, native);
        assert!(restored.fits_source(&source()));
    }
    let mut missing = serde_json::to_value(point()).unwrap();
    missing.as_object_mut().unwrap().remove("auto_shrink");
    assert!(serde_json::from_value::<DatabasePropertiesObservation>(missing).is_err());
    for malformed in [serde_json::json!(2), serde_json::json!("false")] {
        let mut value = serde_json::to_value(point()).unwrap();
        value["auto_shrink"] = malformed;
        assert!(serde_json::from_value::<DatabasePropertiesObservation>(value).is_err());
    }
}

#[test]
fn standby_and_delayed_durability_keep_nullable_native_configuration_and_codes_independent_of_descriptions() {
    for standby in [Some(true), Some(false), None] {
        let mut original = point();
        original.in_standby = standby;
        for (native, expected) in [
            (Some(0), Some(DatabasePropertyState::Off)),
            (Some(1), Some(DatabasePropertyState::Allowed)),
            (Some(2), Some(DatabasePropertyState::Forced)),
            (Some(255), None),
            (None, None),
        ] {
            original.delayed_durability = DatabasePropertyCode { code: native, description: Some("DISABLED".into()) };
            assert!(original.is_valid());
            assert_eq!(original.state(DatabasePropertyCodeKind::DelayedDurability), expected);
            let restored: DatabasePropertiesObservation =
                serde_json::from_slice(&serde_json::to_vec(&original).unwrap()).unwrap();
            assert_eq!(restored.in_standby, standby);
            assert_eq!(restored.delayed_durability, original.delayed_durability);
            assert!(restored.fits_source(&source()));
        }
    }
    for field in ["in_standby", "delayed_durability"] {
        let mut value = serde_json::to_value(point()).unwrap();
        value.as_object_mut().unwrap().remove(field);
        assert!(serde_json::from_value::<DatabasePropertiesObservation>(value).is_err(), "{field}");
    }
}

#[test]
fn service_broker_configuration_preserves_native_true_false_null_and_requires_its_serialized_field() {
    for broker_enabled in [Some(true), Some(false), None] {
        let mut original = point();
        original.broker_enabled = broker_enabled;
        assert!(original.is_valid());
        let restored: DatabasePropertiesObservation =
            serde_json::from_slice(&serde_json::to_vec(&original).unwrap()).unwrap();
        assert_eq!(restored.broker_enabled, broker_enabled);
        assert!(restored.fits_source(&source()));
    }
    let mut missing = serde_json::to_value(point()).unwrap();
    missing.as_object_mut().unwrap().remove("broker_enabled");
    assert!(serde_json::from_value::<DatabasePropertiesObservation>(missing).is_err());
    for malformed in [serde_json::json!(2), serde_json::json!("false")] {
        let mut value = serde_json::to_value(point()).unwrap();
        value["broker_enabled"] = malformed;
        assert!(serde_json::from_value::<DatabasePropertiesObservation>(value).is_err());
    }
}

#[test]
fn feature_and_replication_flags_require_their_nullable_native_contract_without_coercion() {
    for field in [
        "encrypted",
        "ledger_enabled",
        "change_feed_enabled",
        "published",
        "subscribed",
        "merge_published",
        "distributor",
        "accelerated_recovery_enabled",
    ] {
        for native in [serde_json::json!(true), serde_json::json!(false), serde_json::Value::Null] {
            let mut encoded = serde_json::to_value(point()).unwrap();
            encoded[field] = native.clone();
            let decoded: DatabasePropertiesObservation = serde_json::from_value(encoded).unwrap();
            assert!(decoded.is_valid() && decoded.fits_source(&source()));
            assert_eq!(serde_json::to_value(decoded).unwrap()[field], native, "{field}");
        }
        let mut omitted = serde_json::to_value(point()).unwrap();
        omitted.as_object_mut().unwrap().remove(field);
        assert!(serde_json::from_value::<DatabasePropertiesObservation>(omitted).is_err(), "{field}");
        for native in [serde_json::json!(2), serde_json::json!("false"), serde_json::json!([])] {
            let mut encoded = serde_json::to_value(point()).unwrap();
            encoded[field] = native;
            assert!(serde_json::from_value::<DatabasePropertiesObservation>(encoded).is_err(), "{field}");
        }
    }
}

#[test]
fn containment_keeps_the_native_code_separate_from_description_and_availability() {
    let mut point = point();
    for (native, expected) in [
        (Some(0), Some(DatabasePropertyState::NoContainment)),
        (Some(1), Some(DatabasePropertyState::PartialContainment)),
        (Some(255), None),
        (None, None),
    ] {
        point.containment = DatabasePropertyCode { code: native, description: Some("PARTIAL".into()) };
        assert!(point.is_valid());
        assert_eq!(point.state(DatabasePropertyCodeKind::Containment), expected);
        let restored: DatabasePropertiesObservation =
            serde_json::from_slice(&serde_json::to_vec(&point).unwrap()).unwrap();
        assert_eq!(restored.containment, point.containment);
    }
    point.containment = DatabasePropertyCode { code: None, description: None };
    assert!(point.is_valid());
    let mut missing = serde_json::to_value(&point).unwrap();
    missing.as_object_mut().unwrap().remove("containment");
    assert!(serde_json::from_value::<DatabasePropertiesObservation>(missing).is_err());
}

#[test]
fn current_and_catalog_ids_and_created_or_renamed_time_are_independent_native_fields_without_incarnation_proof() {
    let mut point = point();
    point.catalog_database_id = "42".into();
    point.catalog_created_without_offset = Some("2019-12-01T08:30:00.003".into());
    assert!(point.is_valid());
    assert_eq!(point.native_database_id, "5");
    for value in ["", "0", "042", "-1"] {
        let mut changed = point.clone();
        changed.catalog_database_id = value.into();
        assert!(!changed.is_valid(), "{value}");
    }
    for value in ["2019-12-01T08:30:00Z", "2019-12-01T08:30:00+02:00", "2019-02-30T08:30:00"] {
        let mut changed = point.clone();
        changed.catalog_created_without_offset = Some(value.into());
        assert!(!changed.is_valid(), "{value}");
    }
    let mut value = serde_json::to_value(&point).unwrap();
    for field in ["catalog_database_id", "catalog_created_without_offset", "local_replica_role"] {
        let saved = value.as_object_mut().unwrap().remove(field).unwrap();
        assert!(serde_json::from_value::<DatabasePropertiesObservation>(value.clone()).is_err(), "{field}");
        value.as_object_mut().unwrap().insert(field.into(), saved);
    }
}

#[test]
fn native_local_role_keeps_null_and_its_own_original_clock_and_cannot_expand_the_read_interval() {
    let mut read = read();
    let point = read.observation.as_mut().unwrap();
    point.local_replica_role = Some(MssqlDatabaseReplicaRole::LocalAvailabilityGroup {
        source: MssqlDatabaseReplicaRoleSource::HadrDatabaseReplicaStates,
        observed_at: "2020-01-01T12:00:00Z".into(),
        replica_id: "91487244-730d-4b83-8e49-d482241c7288".into(),
        group_database_id: "71bd65c3-bd16-4d08-b5ae-d72e3bf2f6d4".into(),
        is_primary: None,
    });
    assert!(read.is_valid());
    let restored: DatabasePropertiesRead = serde_json::from_value(serde_json::to_value(&read).unwrap()).unwrap();
    assert_eq!(restored, read);
    for at in ["2020-01-01T11:59:59Z", "2020-01-01T12:00:00.0000002Z"] {
        let mut changed = read.clone();
        let Some(MssqlDatabaseReplicaRole::LocalAvailabilityGroup { observed_at, .. }) =
            &mut changed.observation.as_mut().unwrap().local_replica_role
        else {
            panic!()
        };
        *observed_at = at.into();
        assert!(!changed.is_valid(), "{at}");
    }
}

#[test]
fn not_in_group_retains_its_original_verdict_independently_of_later_catalog_state_and_updateability() {
    let mut point = point();
    point.local_replica_role =
        Some(MssqlDatabaseReplicaRole::NotInAvailabilityGroup { observed_at: "2020-01-01T12:00:00Z".into() });
    point.updateability = Some("READ_ONLY".into());
    assert!(point.is_valid());
    point.catalog_database_id = "42".into();
    assert!(!point.is_valid(), "boxed/MI membership does not mix catalog and DMV namespaces");
    point.catalog_database_id = point.native_database_id.clone();
    point.catalog_state.code = None;
    assert!(point.is_valid(), "the earlier native role verdict does not borrow the later catalog state");
    assert!(matches!(point.local_replica_role, Some(MssqlDatabaseReplicaRole::NotInAvailabilityGroup { .. })));
    point.local_replica_role = Some(MssqlDatabaseReplicaRole::Unavailable {
        observed_at: "2020-01-01T12:00:00Z".into(),
        reason: MssqlDatabaseReplicaRoleUnavailable::PermissionDenied,
        native_error_number: Some(229),
    });
    assert!(point.is_valid(), "optional role denial preserves independently available catalog fields");
}
