use super::*;
fn target() -> MssqlObjectIdentity {
    MssqlObjectIdentity { database_id: 7, object_id: 100, created_without_offset: "2019-01-01T00:00:00.0000000".into() }
}
fn point() -> MssqlTableMemory {
    MssqlTableMemory {
        table: target(),
        observed_at: "2020-01-01T00:00:00.5Z".into(),
        allocated_table_kibibytes: Some("9007199254740993".into()),
        used_table_kibibytes: Some("0".into()),
        allocated_indexes_kibibytes: None,
        used_indexes_kibibytes: Some("4".into()),
    }
}
fn source() -> CollectionDescription {
    CollectionDescription {
        target: crate::DatabaseIdentity::server(crate::Engine::Mssql, "owned-db", "original.invalid", 1433, None)
            .into(),
        partition: SourcePartition::InstanceLocal {
            member: Some(SharedResourceIdentity {
                engine: crate::Engine::Mssql,
                kind: SharedResourceKind::Instance,
                namespace: NativeIdentity::new("machine"),
                native: NativeIdentity::new("member"),
            }),
            epoch: Some("original-process-and-database".into()),
        },
        source: MSSQL_TABLE_MEMORY_SOURCE.into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::TableMemory {
            target: Box::new(target()),
            database_writable: true,
            metadata: Box::new(MssqlTableMemoryMetadata {
                observed_at: point().observed_at,
                schema: "dbo".into(),
                name: "Hot".into(),
                memory_optimized: Some(true),
                xtp_supported: Some(true),
                dmv_permission: Some(true),
                metadata_visible: Some(true),
                layout_available: true,
            }),
            snapshot: StateSnapshotDescription {
                dataset: SnapshotDataset::TableMemory,
                observed_from: "2020-01-01T00:00:00Z".into(),
                observed_until: "2020-01-01T00:00:01Z".into(),
                records: 1,
                sources: vec![StateSourceCoverage::collected(MSSQL_TABLE_MEMORY_SOURCE.into(), 1)],
            },
        },
    }
}
#[test]
fn independent_native_kibibytes_preserve_wide_zero_null_and_capture_identity() {
    let point = point();
    let source = source();
    assert!(point.is_valid());
    assert!(point.fits_source(&source));
    assert_eq!(serde_json::from_value::<MssqlTableMemory>(serde_json::to_value(&point).unwrap()).unwrap(), point);
    let row = TelemetryRecord::Operational(OperationalObservation {
        source: MSSQL_TABLE_MEMORY_SOURCE.into(),
        key: point.key().unwrap(),
        fact: OperationalFact::MssqlTableMemory(Box::new(point.clone())),
    });
    let original = ExportedRecord::new(
        &source,
        &CaptureIdentity { observation_id: "original".into(), revision: None },
        row.clone(),
    )
    .unwrap();
    let later =
        ExportedRecord::new(&source, &CaptureIdentity { observation_id: "later".into(), revision: None }, row).unwrap();
    assert_ne!(original.record_id, later.record_id);
    assert_eq!(point.allocated_table_kibibytes.as_deref(), Some("9007199254740993"));
    assert_eq!(point.used_table_kibibytes.as_deref(), Some("0"));
    assert_eq!(point.allocated_indexes_kibibytes, None);
    // Independent gauges do not manufacture an atomic allocated/used relationship.
    let mut independently_sampled = point;
    independently_sampled.allocated_indexes_kibibytes = Some("1".into());
    assert!(independently_sampled.is_valid());
}
#[test]
fn required_nullable_measurements_and_native_feature_facts_cannot_be_defaulted() {
    for field in
        ["allocated_table_kibibytes", "used_table_kibibytes", "allocated_indexes_kibibytes", "used_indexes_kibibytes"]
    {
        let mut json = serde_json::to_value(point()).unwrap();
        json.as_object_mut().unwrap().remove(field);
        assert!(serde_json::from_value::<MssqlTableMemory>(json).is_err(), "{field}");
    }
    let SourceObservation::TableMemory { metadata, .. } = source().observation else { panic!() };
    for field in ["memory_optimized", "xtp_supported", "dmv_permission", "metadata_visible"] {
        let mut json = serde_json::to_value(&metadata).unwrap();
        json.as_object_mut().unwrap().remove(field);
        assert!(serde_json::from_value::<MssqlTableMemoryMetadata>(json).is_err(), "{field}");
    }
    for value in ["-1", "01", "1.5", "9223372036854775808"] {
        let mut point = point();
        point.used_table_kibibytes = Some(value.into());
        assert!(!point.is_valid(), "{value}");
        assert!(point.key().is_err());
    }
}
#[test]
fn selected_header_rejects_wrong_object_source_native_owner_and_clock() {
    for change in [
        "object",
        "database",
        "creation",
        "source",
        "scope",
        "epoch",
        "engine",
        "dataset",
        "time",
        "metadata-time",
        "feature",
        "permission",
    ] {
        let mut source = source();
        match change {
            "source" => source.source = "other".into(),
            "scope" => source.partition = SourcePartition::Target,
            "epoch" => source.partition = SourcePartition::InstanceLocal { member: None, epoch: None },
            "engine" => {
                let CollectionTarget::Database(db) = &mut source.target else { panic!() };
                db.engine = crate::Engine::Postgres;
            }
            _ => {
                let SourceObservation::TableMemory { snapshot, target, metadata, .. } = &mut source.observation else {
                    panic!()
                };
                match change {
                    "object" => target.object_id += 1,
                    "database" => target.database_id += 1,
                    "creation" => target.created_without_offset = "2020-01-01T00:00:00.0000000".into(),
                    "dataset" => snapshot.dataset = SnapshotDataset::Tables,
                    "time" => snapshot.observed_until = snapshot.observed_from.clone(),
                    "metadata-time" => metadata.observed_at = "2020-01-01T00:00:00.4Z".into(),
                    "feature" => metadata.memory_optimized = None,
                    _ => metadata.dmv_permission = Some(false),
                }
            }
        }
        assert!(!point().fits_source(&source), "{change}");
        let row = TelemetryRecord::Operational(OperationalObservation {
            source: MSSQL_TABLE_MEMORY_SOURCE.into(),
            key: point().key().unwrap(),
            fact: OperationalFact::MssqlTableMemory(Box::new(point())),
        });
        assert!(
            ExportedRecord::new(&source, &CaptureIdentity { observation_id: "original".into(), revision: None }, row)
                .is_err(),
            "{change}"
        );
    }
}
#[test]
fn native_selection_is_explicit_and_does_not_widen_generic_snapshot_or_subject_admission() {
    let request = CollectionRequest {
        subject: CollectionSubject::Database,
        selection: CollectionSelection::TableMemory { target: Box::new(target()) },
        budget: CollectionBudget::default(),
    };
    assert!(request.validate().is_ok());
    assert!(RecipeSelector::TableMemory.matches(&request));
    assert!(!RecipeSelector::Collection(request.collection()).matches(&request));
    let mut generic = request.clone();
    generic.selection = CollectionSelection::Snapshot { dataset: SnapshotDataset::TableMemory };
    assert!(generic.validate().is_err());
    let mut changed = request;
    let CollectionSelection::TableMemory { target } = &mut changed.selection else { panic!() };
    target.object_id = 0;
    assert!(changed.validate().is_err());
}
#[test]
fn missing_permission_and_unsupported_metadata_preserve_a_valid_unavailable_header_without_quantities() {
    for flag in ["permission", "feature", "unknown", "layout"] {
        let mut source = source();
        let SourceObservation::TableMemory { snapshot, metadata, .. } = &mut source.observation else { panic!() };
        snapshot.records = 0;
        snapshot.sources = vec![StateSourceCoverage::unavailable(
            MSSQL_TABLE_MEMORY_SOURCE.into(),
            crate::ErrorCode::PermissionDenied,
            None,
        )];
        match flag {
            "permission" => metadata.dmv_permission = Some(false),
            "feature" => metadata.xtp_supported = Some(false),
            "unknown" => metadata.memory_optimized = None,
            _ => metadata.layout_available = false,
        }
        assert!(table_memory_context(&source).is_ok(), "{flag}");
        assert!(source.partition_is_consistent(), "{flag}");
        assert!(!point().fits_source(&source), "{flag}");
    }
}

fn captured(source: CollectionDescription, point: MssqlTableMemory, id: &str) -> CapturedOperationalRead {
    let identity = CaptureIdentity {
        observation_id: id.repeat(32),
        revision: Some(CaptureRevision { outbox_id: "b".repeat(32), owner_epoch: "3".into(), sequence: "4".into() }),
    };
    let capture = DiagnosticCaptureRef {
        capture: identity.clone(),
        manifest_sha256: digest(id.as_bytes()),
        source_scope_id: source.captured_range().unwrap().scope_id,
    };
    let row = OperationalObservation {
        source: MSSQL_TABLE_MEMORY_SOURCE.into(),
        key: point.key().unwrap(),
        fact: OperationalFact::MssqlTableMemory(Box::new(point)),
    };
    let exported = ExportedRecord::new(&source, &identity, TelemetryRecord::Operational(row.clone())).unwrap();
    CapturedOperationalRead::captured(
        source,
        capture,
        vec![CapturedSnapshotPosition::from_export(&exported, 0).unwrap()],
        vec![row],
        true,
    )
    .unwrap()
}
#[test]
fn original_collection_coexists_by_native_object_generation_member_and_capture_and_refuses_conflicts() {
    let first = captured(source(), point(), "a");
    let mut originals = Vec::new();
    append_recorded_operational_read(&mut originals, Box::new(first.clone())).unwrap();
    append_recorded_operational_read(&mut originals, Box::new(first.clone())).unwrap();
    assert_eq!(originals.len(), 1);
    for (id, change) in [("c", "same-object"), ("d", "new-object"), ("e", "recreated-object"), ("f", "other-member")] {
        let mut source = source();
        let mut point = point();
        if change == "other-member" {
            let SourcePartition::InstanceLocal { member: Some(member), .. } = &mut source.partition else { panic!() };
            member.native.id = "another-member".into();
        }
        let SourceObservation::TableMemory { target, .. } = &mut source.observation else { panic!() };
        if change == "new-object" {
            target.object_id += 1;
        }
        if change == "recreated-object" {
            target.created_without_offset = "2020-01-01T00:00:00.0000000".into();
        }
        point.table = target.as_ref().clone();
        append_recorded_operational_read(&mut originals, Box::new(captured(source, point, id))).unwrap();
    }
    assert_eq!(originals.len(), 5);
    let mut different = point();
    different.used_table_kibibytes = Some("1".into());
    let contradiction = captured(source(), different, "a");
    assert_eq!(
        append_recorded_operational_read(&mut originals, Box::new(contradiction)).unwrap_err().detail,
        Some(crate::ErrorDetail::Delivery(DeliveryProblem::Conflict))
    );
    assert_eq!(originals.len(), 5);
    let mut full =
        vec![
            RecordedOperationalRead::Unavailable { reason: crate::message!("view.table_memory.sharing_withheld") };
            MAX_RECORDED_OPERATIONAL_READS
        ];
    let before = full.clone();
    assert_eq!(
        CollectionStop::from_error(&append_recorded_operational_read(&mut full, Box::new(first)).unwrap_err()),
        CollectionStop::RowLimit
    );
    assert_eq!(full, before);
}
#[test]
fn sharing_withdraws_each_original_indivisibly_and_is_idempotent_without_source_owner_or_native_quantities() {
    let mut snapshot: crate::Snapshot =
        serde_json::from_str(include_str!("../../../../../fixtures/mssql-demo.json")).unwrap();
    snapshot.operations.table_memory = vec![
        RecordedOperationalRead::Captured { read: Box::new(captured(source(), point(), "a")) },
        RecordedOperationalRead::Captured { read: Box::new(captured(source(), point(), "c")) },
    ];
    let before = serde_json::to_value(&snapshot.operations.table_memory).unwrap();
    let shared = crate::redact::redact_snapshot(&snapshot);
    assert_eq!(shared.operations.table_memory.len(), 2);
    assert!(
        shared
            .operations
            .table_memory
            .iter()
            .all(|original| matches!(original, RecordedOperationalRead::Unavailable { .. }))
    );
    let text = serde_json::to_string(&shared.operations.table_memory).unwrap();
    for private in
        ["9007199254740993", "original-process-and-database", "member", "created_without_offset", "manifest_sha256"]
    {
        assert!(!text.contains(private), "{private}");
    }
    assert_eq!(crate::redact::redact_snapshot(&shared).operations.table_memory, shared.operations.table_memory);
    assert_eq!(serde_json::to_value(&snapshot.operations.table_memory).unwrap(), before);
}
