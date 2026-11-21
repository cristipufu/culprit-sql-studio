use super::*;
use crate::{Engine, MeasurementScope, SourceState};

fn source() -> CollectionDescription {
    CollectionDescription {
        target: CollectionTarget::Shared(SharedResourceIdentity {
            engine: Engine::Mssql,
            kind: SharedResourceKind::Pool,
            namespace: NativeIdentity::new("logical-server").qualified("kind", "azure-sql-logical-server"),
            native: NativeIdentity::new("original-pool"),
        }),
        partition: SourcePartition::Target,
        source: MSSQL_POOL_SETTINGS_SOURCE.into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::MssqlPoolSettings {
            membership: Box::new(MssqlPoolSettingsMembership {
                database: {
                    let mut database = member_database("logical-server");
                    database.incarnation = Some("11111111-1111-1111-1111-111111111111".into());
                    database
                },
                elastic_pool: Some(SharedResourceIdentity {
                    engine: Engine::Mssql,
                    kind: SharedResourceKind::Pool,
                    namespace: NativeIdentity::new("logical-server").qualified("kind", "azure-sql-logical-server"),
                    native: NativeIdentity::new("original-pool"),
                }),
                elastic_pool_error: None,
                member: NativeIdentity::new("logical-server")
                    .qualified("logical_database_guid", "11111111-1111-1111-1111-111111111111")
                    .qualified("physical_database_guid", "22222222-2222-2222-2222-222222222222"),
                member_observed_at: "2020-01-01T00:00:00Z".into(),
                database_writable: true,
                point: Some(MssqlPoolSettingsPoint { database_id: 7, observed_at: "2020-01-01T00:00:02Z".into() }),
            }),
            snapshot: StateSnapshotDescription {
                dataset: SnapshotDataset::Settings,
                observed_from: "2020-01-01T00:00:01Z".into(),
                observed_until: "2020-01-01T00:00:03Z".into(),
                records: 6,
                sources: vec![StateSourceCoverage::collected(MSSQL_POOL_SETTINGS_SOURCE.into(), 6)],
            },
        },
    }
}
fn setting(name: &str, value: Option<&str>) -> SettingObservation {
    SettingObservation {
        source: MSSQL_POOL_SETTINGS_SOURCE.into(),
        namespace: MSSQL_POOL_SETTINGS_NAMESPACE.into(),
        name: name.into(),
        scope: MeasurementScope::Pool,
        nature: SettingNature::Configuration,
        value: value.map(str::to_owned),
        value_in_use: None,
        value_for_secondary: None,
        unit: mssql_pool_setting_unit(name).map(str::to_owned),
        native_context: None,
        native_source: Some(MSSQL_POOL_SETTINGS_NAMESPACE.into()),
        pending_restart: None,
        native_is_default: None,
        default_value: None,
        default_source: SettingDefaultSource::Unknown,
    }
}
#[test]
fn original_pool_membership_and_current_point_keep_nullable_zero_configuration_and_native_units() {
    let source = source();
    assert!(source.partition_is_consistent());
    for name in MSSQL_POOL_SETTING_NAMES {
        for value in [None, Some("0"), Some("100")] {
            let point = setting(name, value);
            assert!(point.fits_source(&source));
            let record = ExportedRecord::new(
                &source,
                &CaptureIdentity { observation_id: "pool-original".into(), revision: None },
                TelemetryRecord::Setting(point),
            )
            .unwrap();
            assert!(record.record_id.is_some());
        }
    }
    let serialized = serde_json::to_value(&source).unwrap();
    assert_eq!(serde_json::from_value::<CollectionDescription>(serialized).unwrap(), source);
}
#[test]
fn database_group_fields_foreign_units_and_inferred_active_default_or_reset_values_are_refused() {
    let source = source();
    for field in ["cpu_limit", "dtu_limit", "max_memory_grant", "max_dop", "primary_group_max_io", "min_cpu"] {
        let mut value = setting(field, Some("20"));
        if field == "min_cpu" {
            value.scope = MeasurementScope::Database;
        }
        assert!(!value.fits_source(&source), "{field}");
    }
    for mutation in ["unit", "in-use", "default", "context", "restart", "namespace", "source", "measurement"] {
        let mut value = setting("max_memory", Some("75"));
        match mutation {
            "unit" => value.unit = Some("KiB".into()),
            "in-use" => value.value_in_use = Some("75".into()),
            "default" => {
                value.native_is_default = Some(true);
                value.default_value = Some("75".into());
                value.default_source = SettingDefaultSource::NativeFlag;
            }
            "context" => value.native_context = Some("guessed-pool-0".into()),
            "restart" => value.pending_restart = Some(false),
            "namespace" => value.namespace = "sys.dm_resource_governor_workload_groups".into(),
            "source" => value.source = "another-source".into(),
            _ => value.nature = SettingNature::Measurement,
        }
        assert!(!value.fits_source(&source), "{mutation}");
    }
}
#[test]
fn canonical_native_integers_reject_negative_fractional_nonfinite_overflow_and_out_of_range_percentages() {
    for value in ["-1", "1.0", "NaN", "inf", "01", "+1", "101", "4294967296"] {
        assert!(!setting("min_cpu", Some(value)).fits_source(&source()), "{value}");
    }
    assert!(setting("pool_max_io", Some("2147483647")).fits_source(&source()));
    assert!(!setting("pool_max_io", Some("2147483648")).fits_source(&source()));
}
#[test]
fn native_member_guids_selected_point_role_and_clock_are_required_without_target_or_zero_substitutes() {
    for mutation in [
        "engine",
        "pool",
        "server",
        "logical",
        "physical",
        "role",
        "dbid",
        "point-before",
        "point-after",
        "member-after",
        "partition",
    ] {
        let mut value = source();
        match mutation {
            "engine" => {
                let CollectionTarget::Shared(pool) = &mut value.target else { panic!() };
                pool.engine = Engine::Postgres;
            }
            "pool" => {
                let CollectionTarget::Shared(pool) = &mut value.target else { panic!() };
                pool.kind = SharedResourceKind::Instance;
            }
            "server" => {
                let CollectionTarget::Shared(pool) = &mut value.target else { panic!() };
                pool.namespace.id = "foreign-server".into();
            }
            "partition" => value.partition = SourcePartition::InstanceLocal { member: None, epoch: None },
            _ => {
                let SourceObservation::MssqlPoolSettings { membership, .. } = &mut value.observation else { panic!() };
                match mutation {
                    "logical" => {
                        membership.member.qualifiers.remove("logical_database_guid");
                    }
                    "physical" => {
                        membership
                            .member
                            .qualifiers
                            .insert("physical_database_guid".into(), "00000000-0000-0000-0000-000000000000".into());
                    }
                    "role" => membership.database_writable = false,
                    "dbid" => membership.point.as_mut().unwrap().database_id = 0,
                    "point-before" => membership.point.as_mut().unwrap().observed_at = "2020-01-01T00:00:00Z".into(),
                    "point-after" => membership.point.as_mut().unwrap().observed_at = "2020-01-01T00:00:04Z".into(),
                    _ => membership.member_observed_at = "2020-01-01T00:00:02Z".into(),
                }
            }
        }
        assert!(!setting("min_cpu", Some("0")).fits_source(&value), "{mutation}");
    }
}
#[test]
fn unavailable_layout_or_permission_keeps_original_membership_and_cannot_render_missing_point_as_empty() {
    let mut value = source();
    let SourceObservation::MssqlPoolSettings { snapshot, membership } = &mut value.observation else { panic!() };
    membership.point = None;
    snapshot.records = 0;
    snapshot.sources = vec![StateSourceCoverage::unavailable(
        MSSQL_POOL_SETTINGS_SOURCE.into(),
        crate::ErrorCode::PermissionDenied,
        None,
    )];
    assert!(value.partition_is_consistent());
    assert!(!setting("min_cpu", None).fits_source(&value));
    let SourceObservation::MssqlPoolSettings { snapshot, .. } = &mut value.observation else { panic!() };
    snapshot.sources[0].state = SourceState::Empty;
    assert!(!value.partition_is_consistent());
}
#[test]
fn plain_snapshot_and_omitted_nullable_point_cannot_assert_pool_membership() {
    let mut value = source();
    let snapshot = value.observation.snapshot().unwrap().clone();
    value.observation = SourceObservation::Snapshot(snapshot);
    assert!(!setting("min_cpu", Some("0")).fits_source(&value));
    let membership = mssql_pool_settings_membership(&source()).unwrap().unwrap().clone();
    let mut encoded = serde_json::to_value(membership).unwrap();
    encoded.as_object_mut().unwrap().remove("point");
    assert!(serde_json::from_value::<MssqlPoolSettingsMembership>(encoded).is_err());
}

fn member_database(server: &str) -> crate::DatabaseIdentity {
    crate::DatabaseIdentity::server(Engine::Mssql, "recorded-member", server, 1433, None)
}

#[test]
fn database_originals_keep_native_pool_scope_with_or_without_elastic_membership() {
    for pooled in [false, true] {
        let mut original = source();
        let SourceObservation::MssqlPoolSettings { membership, .. } = &mut original.observation else { panic!() };
        if !pooled {
            membership.elastic_pool = None;
        }
        original.target = membership.database.clone().into();
        assert!(original.partition_is_consistent());
        assert!(setting("min_cpu", Some("0")).fits_source(&original));
        let request = CollectionRequest {
            subject: CollectionSubject::Database,
            selection: CollectionSelection::ResourcePoolSettings,
            budget: CollectionBudget::default(),
        };
        assert!(original.matches_request(&request));
        let ordinary = CollectionRequest {
            selection: CollectionSelection::Snapshot { dataset: SnapshotDataset::Settings },
            ..request
        };
        assert!(!original.matches_request(&ordinary));
        let CollectionTarget::Database(database) = &mut original.target else { panic!() };
        database.incarnation = Some("33333333-3333-3333-3333-333333333333".into());
        assert!(!setting("min_cpu", Some("0")).fits_source(&original));
    }
}
#[test]
fn absent_elastic_membership_cannot_authenticate_a_shared_pool_or_be_omitted_from_the_header() {
    let mut original = source();
    let SourceObservation::MssqlPoolSettings { membership, .. } = &mut original.observation else { panic!() };
    membership.elastic_pool = None;
    assert!(!original.partition_is_consistent());
    let SourceObservation::MssqlPoolSettings { membership, .. } = &original.observation else { panic!() };
    let mut encoded = serde_json::to_value(membership.as_ref()).unwrap();
    encoded.as_object_mut().unwrap().remove("elastic_pool");
    assert!(serde_json::from_value::<MssqlPoolSettingsMembership>(encoded).is_err());
    let mut original = source();
    let CollectionTarget::Shared(pool) = &mut original.target else { panic!() };
    pool.native.id = "another-pool".into();
    assert!(!original.partition_is_consistent());
}

#[test]
fn optional_visibility_failure_is_valid_only_for_a_database_target_without_an_asserted_elastic_pool() {
    let mut source = source();
    let SourceObservation::MssqlPoolSettings { membership, .. } = &mut source.observation else { panic!() };
    membership.elastic_pool_error = Some(crate::ErrorCode::PermissionDenied);
    assert!(!membership.is_valid());
    membership.elastic_pool = None;
    source.target = membership.database.clone().into();
    assert!(source.partition_is_consistent());
    assert!(setting("min_cpu", Some("0")).fits_source(&source));
    let mut encoded = serde_json::to_value(&source).unwrap();
    encoded["observation"]["data"]["membership"].as_object_mut().unwrap().remove("elastic_pool_error");
    assert!(serde_json::from_value::<CollectionDescription>(encoded).is_err());
}
