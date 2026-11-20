use super::*;

pub(super) fn target() -> MssqlColumnstoreTarget {
    MssqlColumnstoreTarget {
        table: MssqlObjectIdentity {
            database_id: 7,
            object_id: 100,
            created_without_offset: "2020-01-01T00:00:00.000".into(),
        },
        index_id: 1,
        table_modified_without_offset: "2020-01-01T00:00:00.000".into(),
        partition_number: None,
    }
}
fn context() -> NativeReadContext {
    NativeReadContext {
        target: crate::DatabaseIdentity::server(crate::Engine::Mssql, "owned-db", "original.invalid", 1433, None)
            .into(),
        partition: SourcePartition::InstanceLocal {
            member: Some(SharedResourceIdentity {
                engine: crate::Engine::Mssql,
                kind: SharedResourceKind::Instance,
                namespace: NativeIdentity::new("machine"),
                native: NativeIdentity::new("native-member"),
            }),
            epoch: Some("original-process-and-database".into()),
        },
        observed_from: "2020-01-01T00:00:00Z".into(),
        observed_until: "2020-01-01T00:00:01Z".into(),
        database_writable: Some(true),
    }
}
fn point() -> MssqlColumnstoreRowgroup {
    MssqlColumnstoreRowgroup {
        context: context(),
        target: target(),
        observed_at: "2020-01-01T00:00:00.5Z".into(),
        partition_number: 1,
        row_group_id: 0,
        index_type: 5,
        delta_store_hobt_id: Some("9007199254740993".into()),
        state: 3,
        total_rows: Some("2".into()),
        deleted_rows: Some("0".into()),
        size_bytes: Some("9007199254740993".into()),
    }
}
fn source(point: &MssqlColumnstoreRowgroup) -> CollectionDescription {
    CollectionDescription {
        target: point.context.target.clone(),
        partition: point.context.partition.clone(),
        source: MSSQL_COLUMNSTORE_ROWGROUPS_SOURCE.into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::ColumnstoreRowgroups {
            target: Box::new(point.target.clone()),
            index_type: 5,
            database_writable: true,
            snapshot: StateSnapshotDescription {
                dataset: SnapshotDataset::ColumnstoreRowgroups,
                observed_from: point.context.observed_from.clone(),
                observed_until: point.context.observed_until.clone(),
                records: 1,
                sources: vec![StateSourceCoverage::collected(MSSQL_COLUMNSTORE_ROWGROUPS_SOURCE.into(), 1)],
            },
        },
    }
}
#[test]
fn exact_quantities_nulls_zero_and_original_source_roundtrip_without_rowgroup_continuity() {
    let point = point();
    let source = source(&point);
    let record = TelemetryRecord::Operational(OperationalObservation {
        source: MSSQL_COLUMNSTORE_ROWGROUPS_SOURCE.into(),
        key: point.key().unwrap(),
        fact: OperationalFact::MssqlColumnstoreRowgroup(Box::new(point.clone())),
    });
    let first = ExportedRecord::new(
        &source,
        &CaptureIdentity { observation_id: "original".into(), revision: None },
        record.clone(),
    )
    .unwrap();
    let later =
        ExportedRecord::new(&source, &CaptureIdentity { observation_id: "later".into(), revision: None }, record)
            .unwrap();
    assert_ne!(first.record_id, later.record_id);
    assert_eq!(serde_json::from_value::<ExportedRecord>(serde_json::to_value(&first).unwrap()).unwrap(), first);
    assert!(point.fits_source(&source));
    assert_eq!(point.size_bytes.as_deref(), Some("9007199254740993"));
    assert_eq!(point.deleted_rows.as_deref(), Some("0"));
}
#[test]
fn native_scope_target_role_source_family_and_clock_contradictions_are_refused() {
    let point = point();
    for change in ["vendor", "member", "epoch", "role", "target", "dataset", "clock", "source"] {
        let mut source = source(&point);
        match change {
            "vendor" => {
                let CollectionTarget::Database(db) = &mut source.target else { panic!() };
                db.engine = crate::Engine::Postgres;
            }
            "member" => {
                let SourcePartition::InstanceLocal { member: Some(member), .. } = &mut source.partition else {
                    panic!()
                };
                member.native.id = "other".into();
            }
            "epoch" => source.partition = SourcePartition::InstanceLocal { member: None, epoch: None },
            "source" => source.source = "other".into(),
            _ => {
                let SourceObservation::ColumnstoreRowgroups { snapshot, target, database_writable, .. } =
                    &mut source.observation
                else {
                    panic!()
                };
                match change {
                    "role" => *database_writable = false,
                    "target" => target.index_id = 2,
                    "dataset" => snapshot.dataset = SnapshotDataset::Indexes,
                    _ => snapshot.observed_until = "2020-01-01T00:00:00Z".into(),
                }
            }
        }
        assert!(!point.fits_source(&source), "{change}");
    }
}
#[test]
fn invalid_native_states_populations_partitions_and_unrepresentable_quantities_are_not_healthy_zero() {
    for change in ["state", "kind", "partition", "deleted", "negative", "fractional", "wide", "clock", "null-role"] {
        let mut point = point();
        match change {
            "state" => point.state = 5,
            "kind" => point.index_type = 1,
            "partition" => point.target.partition_number = Some(2),
            "deleted" => point.deleted_rows = Some("3".into()),
            "negative" => point.total_rows = Some("-1".into()),
            "fractional" => point.total_rows = Some("1.5".into()),
            "wide" => point.size_bytes = Some("9223372036854775808".into()),
            "clock" => point.observed_at = "2020-01-01T00:00:02Z".into(),
            _ => point.context.database_writable = None,
        }
        assert!(!point.is_valid(), "{change}");
        assert!(point.key().is_err());
    }
}
#[test]
fn explicit_target_is_required_and_does_not_enable_an_unselected_collection() {
    let request = CollectionRequest {
        subject: CollectionSubject::Database,
        selection: CollectionSelection::ColumnstoreRowgroups { target: Box::new(target()) },
        budget: CollectionBudget::default(),
    };
    assert!(request.validate().is_ok());
    assert!(RecipeSelector::ColumnstoreRowgroups.matches(&request));
    assert!(!RecipeSelector::Collection(request.collection()).matches(&request));
    let mut broad = request.clone();
    broad.selection = CollectionSelection::Snapshot { dataset: SnapshotDataset::ColumnstoreRowgroups };
    assert!(broad.validate().is_err());
    let mut invalid = target();
    invalid.partition_number = Some(0);
    assert!(!invalid.is_valid());
}

#[test]
fn required_nullable_native_quantities_cannot_be_silently_filled_from_missing_json_fields() {
    let original = point();
    for field in ["delta_store_hobt_id", "total_rows", "deleted_rows", "size_bytes"] {
        let mut value = serde_json::to_value(&original).unwrap();
        value.as_object_mut().unwrap().remove(field);
        assert!(serde_json::from_value::<MssqlColumnstoreRowgroup>(value).is_err(), "{field}");
    }
    let mut value = serde_json::to_value(&original).unwrap();
    value["size_bytes"] = serde_json::Value::Null;
    assert!(serde_json::from_value::<MssqlColumnstoreRowgroup>(value).unwrap().size_bytes.is_none());
}
