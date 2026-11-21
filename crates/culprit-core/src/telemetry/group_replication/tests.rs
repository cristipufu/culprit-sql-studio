use super::*;
fn point() -> MysqlGroupReplicationPoint {
    MysqlGroupReplicationPoint::Statistics(Box::new(MysqlGroupStatistics {
        group_id: "group-native".into(),
        member_id: "remote-member".into(),
        channel: "group_replication_applier".into(),
        observed_at: "2020-01-01T00:00:00.5Z".into(),
        view_id: Some("view-original".into()),
        certification_queue: Some("0".into()),
        checked: Some("18446744073709551615".into()),
        conflicts: Some("9007199254740993".into()),
        rows_validating: None,
        applier_queue: Some("3".into()),
        remote_applied: None,
        local_proposed: Some("17".into()),
        local_rollback: Some("1".into()),
    }))
}
fn source() -> CollectionDescription {
    let member = SharedResourceIdentity {
        engine: crate::Engine::Mysql,
        kind: SharedResourceKind::Instance,
        namespace: NativeIdentity::new("mysql-server"),
        native: NativeIdentity::new("original-member"),
    };
    CollectionDescription {
        target: CollectionTarget::Shared(member.clone()),
        partition: SourcePartition::InstanceLocal { member: Some(member), epoch: Some("original-process".into()) },
        source: MYSQL_GROUP_REPLICATION_SOURCE.into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::GroupReplication {
            metadata: Box::new(MysqlGroupReplicationMetadata {
                observed_at: "2020-01-01T00:00:00Z".into(),
                group_id: Some("group-native".into()),
                plugin_status: Some("ACTIVE".into()),
                members_layout: true,
                statistics_layout: true,
            }),
            snapshot: StateSnapshotDescription {
                dataset: SnapshotDataset::GroupReplication,
                observed_from: "2020-01-01T00:00:00Z".into(),
                observed_until: "2020-01-01T00:00:01Z".into(),
                records: 1,
                sources: vec![
                    StateSourceCoverage::observed(MYSQL_GROUP_MEMBERS_SOURCE.into(), 0, true),
                    StateSourceCoverage::collected(MYSQL_GROUP_STATISTICS_SOURCE.into(), 1),
                ],
            },
        },
    }
}
fn row(point: MysqlGroupReplicationPoint) -> TelemetryRecord {
    TelemetryRecord::Operational(OperationalObservation {
        source: point.source().into(),
        key: point.key().unwrap(),
        fact: OperationalFact::MysqlGroupReplication(Box::new(point)),
    })
}
#[test]
fn native_group_points_keep_wide_integer_null_zero_and_independent_member_identity() {
    let point = point();
    let source = source();
    assert!(point.fits_source(&source));
    assert_eq!(
        serde_json::from_value::<MysqlGroupReplicationPoint>(serde_json::to_value(&point).unwrap()).unwrap(),
        point
    );
    let capture = CaptureIdentity { observation_id: "original".into(), revision: None };
    let exported = ExportedRecord::new(&source, &capture, row(point.clone())).unwrap();
    assert_eq!(exported.record, row(point));
    assert_eq!(NativeReadContext::from_snapshot(&source).unwrap().database_writable, None);
    assert!(source.target.database().is_none());
}
#[test]
fn malformed_counters_and_missing_required_nullable_fields_cannot_become_zero() {
    for value in ["-1", "1.5", "01", "NaN", "18446744073709551616", ""] {
        let mut point = point();
        let MysqlGroupReplicationPoint::Statistics(stats) = &mut point else { panic!() };
        stats.conflicts = Some(value.into());
        assert!(!point.is_valid(), "{value}");
    }
    for field in ["certification_queue", "conflicts", "applier_queue", "view_id"] {
        let mut json = serde_json::to_value(point()).unwrap();
        json["data"].as_object_mut().unwrap().remove(field);
        assert!(serde_json::from_value::<MysqlGroupReplicationPoint>(json).is_err(), "{field}");
    }
}
#[test]
fn raw_export_refuses_foreign_group_scope_member_epoch_clock_layout_and_native_family() {
    for change in
        ["target", "member", "epoch", "engine", "group", "clock", "plugin", "layout", "dataset", "generic", "source"]
    {
        let mut source = source();
        match change {
            "target" => {
                source.target =
                    crate::DatabaseIdentity::server(crate::Engine::Mysql, "db", "route.invalid", 3306, None).into()
            }
            "member" => {
                let SourcePartition::InstanceLocal { member: Some(member), .. } = &mut source.partition else {
                    panic!()
                };
                member.native.id = "other-member".into();
            }
            "epoch" => {
                let SourcePartition::InstanceLocal { epoch, .. } = &mut source.partition else { panic!() };
                *epoch = None;
            }
            "engine" => {
                let CollectionTarget::Shared(target) = &mut source.target else { panic!() };
                target.engine = crate::Engine::Postgres;
                let SourcePartition::InstanceLocal { member: Some(member), .. } = &mut source.partition else {
                    panic!()
                };
                member.engine = crate::Engine::Postgres;
            }
            "source" => source.source = "replication-channels".into(),
            _ => {
                let SourceObservation::GroupReplication { metadata, snapshot } = &mut source.observation else {
                    panic!()
                };
                match change {
                    "group" => metadata.group_id = Some("foreign-group".into()),
                    "clock" => snapshot.observed_until = snapshot.observed_from.clone(),
                    "plugin" => metadata.plugin_status = None,
                    "layout" => metadata.statistics_layout = false,
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
                row(point())
            )
            .is_err(),
            "{change}"
        );
    }
}
#[test]
fn known_shared_instance_selection_is_required_without_database_scope_from_names() {
    let source = source();
    let CollectionTarget::Shared(resource) = source.target else { panic!() };
    let request = CollectionRequest {
        subject: CollectionSubject::Shared { resource },
        selection: CollectionSelection::Snapshot { dataset: SnapshotDataset::GroupReplication },
        budget: CollectionBudget::default(),
    };
    assert!(request.validate().is_ok());
    let mut wrong = request.clone();
    wrong.subject = CollectionSubject::Database;
    assert!(wrong.validate().is_err());
    let mut wrong = request;
    let CollectionSubject::Shared { resource } = &mut wrong.subject else { panic!() };
    resource.kind = SharedResourceKind::Pool;
    assert!(wrong.validate().is_err());
}
#[test]
fn sharing_withdraws_each_original_instead_of_preserving_native_ownership_or_a_healthy_zero() {
    let mut snapshot: crate::Snapshot =
        serde_json::from_str(include_str!("../../../../../fixtures/mysql-demo.json")).unwrap();
    snapshot.operations.group_replication =
        vec![RecordedOperationalRead::Unavailable { reason: crate::message!("view.group_replication.invalid") }];
    let shared = crate::redact::redact_snapshot(&snapshot);
    assert!(
        matches!(&shared.operations.group_replication[0],RecordedOperationalRead::Unavailable{reason} if reason==&crate::message!("view.group_replication.sharing_withheld"))
    );
    assert_eq!(
        crate::redact::redact_snapshot(&shared).operations.group_replication,
        shared.operations.group_replication
    );
}
