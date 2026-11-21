use super::*;
fn point() -> MariadbGaleraPoint {
    MariadbGaleraPoint {
        observed_from: "2020-01-01T00:00:00Z".into(),
        observed_until: "2020-01-01T00:00:01Z".into(),
        available_variables: MARIADB_GALERA_FIELDS.map(str::to_owned).into(),
        ready: Some(true),
        connected: Some(true),
        component_status: Some("Primary".into()),
        local_state: Some("4".into()),
        local_state_comment: Some("Synced".into()),
        cluster_size: Some("3".into()),
        cluster_state_uuid: Some("aaaaaaaa-0000-4000-8000-000000000001".into()),
        local_state_uuid: Some("aaaaaaaa-0000-4000-8000-000000000001".into()),
        node_uuid: Some("bbbbbbbb-0000-4000-8000-000000000002".into()),
        receive_queue: Some("2".into()),
        send_queue: Some("3".into()),
        certification_failures: Some("9007199254740993".into()),
        applier_aborts: Some("7".into()),
        replays: Some("11".into()),
        local_commits: Some("18446744073709551615".into()),
        paused_fraction: Some("0.125000".into()),
        paused_nanoseconds: Some("123456789".into()),
    }
}
fn source() -> CollectionDescription {
    let instance = SharedResourceIdentity {
        engine: crate::Engine::Mysql,
        kind: SharedResourceKind::Instance,
        namespace: NativeIdentity::new("mariadb-server-uid"),
        native: NativeIdentity::new("iFL88RG7XPipmQ5HOnA9iLqJw6w="),
    };
    CollectionDescription {
        target: CollectionTarget::Shared(instance.clone()),
        partition: SourcePartition::InstanceLocal { member: Some(instance), epoch: Some("original-process".into()) },
        source: MARIADB_GALERA_SOURCE.into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::Galera {
            metadata: Box::new(MariadbGaleraMetadata {
                observed_at: "2020-01-01T00:00:00Z".into(),
                enabled: Some(true),
            }),
            snapshot: StateSnapshotDescription {
                dataset: SnapshotDataset::Galera,
                observed_from: "2020-01-01T00:00:00Z".into(),
                observed_until: "2020-01-01T00:00:01Z".into(),
                records: 1,
                sources: vec![StateSourceCoverage::collected(MARIADB_GALERA_SOURCE.into(), 1)],
            },
        },
    }
}
fn record(point: MariadbGaleraPoint) -> TelemetryRecord {
    TelemetryRecord::Operational(OperationalObservation {
        source: MARIADB_GALERA_SOURCE.into(),
        key: point.key().unwrap(),
        fact: OperationalFact::MariadbGalera(Box::new(point)),
    })
}
#[test]
fn native_point_preserves_wide_totals_current_queues_nulls_and_required_availability() {
    let point = point();
    assert!(point.fits_source(&source()));
    assert_eq!(serde_json::from_value::<MariadbGaleraPoint>(serde_json::to_value(&point).unwrap()).unwrap(), point);
    let mut future_state = point.clone();
    future_state.local_state = Some("-1".into());
    future_state.component_status = Some("future-native-state".into());
    future_state.node_uuid = Some(String::new());
    assert!(future_state.is_valid());
    let mut unknown = point.clone();
    unknown.receive_queue = None;
    assert!(unknown.is_valid());
    let mut absent = unknown.clone();
    absent.available_variables.retain(|name| name != "wsrep_local_recv_queue");
    assert!(absent.is_valid());
    absent.receive_queue = Some("0".into());
    assert!(!absent.is_valid());
    for field in ["ready", "receive_queue", "paused_fraction", "node_uuid"] {
        let mut json = serde_json::to_value(&point).unwrap();
        json.as_object_mut().unwrap().remove(field);
        assert!(serde_json::from_value::<MariadbGaleraPoint>(json).is_err(), "{field}");
    }
    for value in ["-1", "01", "18446744073709551616", "1.5"] {
        let mut invalid = point.clone();
        invalid.receive_queue = Some(value.into());
        assert!(!invalid.is_valid(), "{value}");
    }
    for value in ["NaN", "inf", "1.01", "-0.1"] {
        let mut invalid = point.clone();
        invalid.paused_fraction = Some(value.into());
        assert!(!invalid.is_valid(), "{value}");
    }
}
#[test]
fn raw_original_refuses_foreign_owner_epoch_clock_configuration_dataset_and_source() {
    for change in [
        "database",
        "member",
        "engine",
        "namespace",
        "epoch",
        "clock",
        "disabled",
        "unknown",
        "dataset",
        "generic",
        "source",
    ] {
        let mut source = source();
        match change {
            "database" => {
                source.target =
                    crate::DatabaseIdentity::server(crate::Engine::Mysql, "db", "route.invalid", 3306, None).into()
            }
            "member" => {
                if let SourcePartition::InstanceLocal { member: Some(member), .. } = &mut source.partition {
                    member.native.id = "other".into();
                }
            }
            "engine" | "namespace" => {
                let CollectionTarget::Shared(target) = &mut source.target else { panic!() };
                let SourcePartition::InstanceLocal { member: Some(member), .. } = &mut source.partition else {
                    panic!()
                };
                if change == "engine" {
                    target.engine = crate::Engine::Postgres;
                    member.engine = crate::Engine::Postgres;
                } else {
                    target.namespace.id = "mysql-server".into();
                    member.namespace.id = "mysql-server".into();
                }
            }
            "epoch" => {
                if let SourcePartition::InstanceLocal { epoch, .. } = &mut source.partition {
                    *epoch = None;
                }
            }
            "source" => source.source = "status".into(),
            _ => {
                let SourceObservation::Galera { metadata, snapshot } = &mut source.observation else { panic!() };
                match change {
                    "clock" => snapshot.observed_until = snapshot.observed_from.clone(),
                    "disabled" => metadata.enabled = Some(false),
                    "unknown" => metadata.enabled = None,
                    "dataset" => snapshot.dataset = SnapshotDataset::Operational,
                    "generic" => source.observation = SourceObservation::Snapshot(snapshot.clone()),
                    _ => unreachable!(),
                }
            }
        }
        assert!(
            ExportedRecord::new(
                &source,
                &CaptureIdentity { observation_id: "original".into(), revision: None },
                record(point())
            )
            .is_err(),
            "{change}"
        );
    }
}
#[test]
fn galera_requires_explicit_shared_instance_selection_and_stays_an_unverified_snapshot() {
    let description = source();
    let CollectionTarget::Shared(resource) = description.target else { panic!() };
    let request = CollectionRequest {
        subject: CollectionSubject::Shared { resource },
        selection: CollectionSelection::Snapshot { dataset: SnapshotDataset::Galera },
        budget: CollectionBudget::default(),
    };
    assert!(request.validate().is_ok());
    assert!(source().partition_is_consistent());
    let mut wrong = request;
    wrong.subject = CollectionSubject::Database;
    assert!(wrong.validate().is_err());
}

#[test]
fn enabled_missing_status_cannot_be_sealed_as_an_empty_or_complete_healthy_population() {
    let mut description = source();
    let SourceObservation::Galera { snapshot, .. } = &mut description.observation else { panic!() };
    snapshot.records = 0;
    snapshot.sources[0] = StateSourceCoverage::observed(MARIADB_GALERA_SOURCE.into(), 0, true);
    assert!(galera_context(&description).is_err());
    let mut incomplete = point();
    incomplete.available_variables.retain(|name| name != "wsrep_local_recv_queue");
    incomplete.receive_queue = None;
    assert!(incomplete.is_valid());
    assert!(!incomplete.fits_source(&source()));
}
