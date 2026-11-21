use super::*;
mod shared_tempdb_log;
fn rows() -> Vec<OperationalObservation> {
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/storage.native.json")))
            .unwrap();
    let space = serde_json::from_value(fixture["space"].clone()).unwrap();
    let file: MssqlDatabaseFile = serde_json::from_value(fixture["file"].clone()).unwrap();
    vec![
        OperationalObservation {
            source: crate::source_id::FILE_CAPACITY.into(),
            key: "filegroup:1".into(),
            fact: OperationalFact::Space(space),
        },
        OperationalObservation {
            source: crate::source_id::FILE_CAPACITY.into(),
            key: file.key().unwrap(),
            fact: OperationalFact::MssqlDatabaseFile(Box::new(file)),
        },
    ]
}
fn read() -> StorageRead {
    StorageRead {
        target: crate::DatabaseIdentity::server(crate::Engine::Mssql, "sample", "fixture.invalid", 1433, None).into(),
        context: ObservationReadContext::Collector {
            observed_from: "2020-01-01T12:00:00Z".into(),
            observed_until: "2020-01-01T12:00:01Z".into(),
        },
        sources: vec![StateSourceCoverage::collected(crate::source_id::FILE_CAPACITY.into(), 2)],
        observations: rows(),
        read_complete: true,
    }
}
fn snapshot_source() -> CollectionDescription {
    let read = read();
    CollectionDescription {
        target: read.target,
        partition: SourcePartition::Target,
        source: "operational-state".into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::Snapshot(StateSnapshotDescription {
            dataset: SnapshotDataset::Operational,
            observed_from: "2020-01-01T12:00:00Z".into(),
            observed_until: "2020-01-01T12:00:01Z".into(),
            records: 2,
            sources: read.sources,
        }),
    }
}
fn paged_source() -> CollectionDescription {
    let mut source = snapshot_source();
    source.observation = SourceObservation::PagedSnapshot(PagedSnapshotDescription {
        dataset: SnapshotDataset::Operational,
        observed_from: "2020-01-01T12:00:00Z".into(),
        sources: vec![crate::source_id::FILE_CAPACITY.into()],
    });
    source
}

#[test]
fn paged_storage_accepts_native_points_after_its_start_but_does_not_claim_closed_coverage() {
    let open = paged_source();
    let capture = CaptureIdentity { observation_id: "paged-storage-admission".into(), revision: None };
    let points = rows();
    for point in &points {
        assert!(ExportedRecord::new(&open, &capture, TelemetryRecord::Operational(point.clone())).is_ok());
    }
    assert!(!open.observation.covers_request());
    assert!(StorageRead::from_source(&open, points.clone(), true).is_err());
    let SourceObservation::Snapshot(completion) = snapshot_source().observation else { panic!() };
    let closed = open.completed(SourceCompletion::Snapshot(completion)).unwrap().unwrap();
    assert!(StorageRead::from_source(&closed, points, true).unwrap().is_valid());
}

#[test]
fn storage_rows_must_fit_both_final_bounds_even_when_the_open_source_accepted_them() {
    let capture = CaptureIdentity { observation_id: "storage-final-bounds".into(), revision: None };
    for boundary in ["before-start", "after-end", "point"] {
        let mut source = snapshot_source();
        let SourceObservation::Snapshot(header) = &mut source.observation else { panic!() };
        match boundary {
            "before-start" => header.observed_from = "2020-01-01T12:00:00.6Z".into(),
            "after-end" => header.observed_until = "2020-01-01T12:00:00.4Z".into(),
            _ => {
                header.observed_from = "2020-01-01T12:00:00.5Z".into();
                header.observed_until = header.observed_from.clone();
            }
        }
        for point in rows() {
            assert_eq!(
                ExportedRecord::new(&source, &capture, TelemetryRecord::Operational(point)).is_ok(),
                boundary == "point",
                "{boundary}"
            );
        }
    }
}

#[test]
fn paged_storage_refuses_points_before_start_foreign_databases_and_wrong_or_invalid_headers() {
    let capture = CaptureIdentity { observation_id: "paged-storage-refusal".into(), revision: None };
    for change in ["before-start", "foreign-database", "wrong-dataset", "invalid-start", "undeclared-source"] {
        let mut source = paged_source();
        let SourceObservation::PagedSnapshot(header) = &mut source.observation else { panic!() };
        match change {
            "before-start" => header.observed_from = "2020-01-01T12:00:00.6Z".into(),
            "wrong-dataset" => header.dataset = SnapshotDataset::Sequences,
            "invalid-start" => header.observed_from = "2020-01-01T12:00:00.0000000001Z".into(),
            "undeclared-source" => header.sources = vec![crate::source_id::LOG_CAPACITY.into()],
            _ => {
                source.target =
                    crate::DatabaseIdentity::server(crate::Engine::Mssql, "foreign", "fixture.invalid", 1433, None)
                        .into();
            }
        }
        for point in rows() {
            assert!(ExportedRecord::new(&source, &capture, TelemetryRecord::Operational(point)).is_err(), "{change}");
        }
    }
}
#[test]
fn exact_storage_serialization_preserves_native_values_and_each_declared_population_without_owner_inference() {
    let read = read();
    assert!(read.is_valid());
    let restored: StorageRead = serde_json::from_slice(&serde_json::to_vec(&read).unwrap()).unwrap();
    assert_eq!(restored, read);
    let OperationalFact::Space(space) = &restored.observations[0].fact else { panic!() };
    assert_eq!(space.total_bytes.as_deref(), Some("9007199254740993"));
    assert_eq!(space.used_bytes.as_deref(), Some("0"));
    assert!(restored.context.native().is_none());
}
#[test]
fn unknown_values_incomplete_bodies_and_unavailable_sources_are_distinct_from_measured_zero() {
    let mut read = read();
    read.observations.remove(0);
    read.read_complete = false;
    assert!(read.is_valid());
    assert_eq!(read.sources[0].records, 2);
    read.read_complete = true;
    assert!(!read.is_valid());
    read.observations.clear();
    read.sources[0] = StateSourceCoverage::unavailable(
        crate::source_id::FILE_CAPACITY.into(),
        crate::ErrorCode::PermissionDenied,
        None,
    );
    assert!(read.is_valid());
}
#[test]
fn duplicate_keys_foreign_namespaces_wrong_populations_negative_or_approximate_quantities_are_refused() {
    for invalid in ["duplicate", "foreign", "population", "negative", "float"] {
        let mut read = read();
        match invalid {
            "duplicate" => {
                read.observations[1] = read.observations[0].clone();
            }
            "foreign" => {
                let OperationalFact::MssqlDatabaseFile(file) = &mut read.observations[1].fact else { panic!() };
                file.database = "foreign".into();
            }
            change => {
                let OperationalFact::Space(space) = &mut read.observations[0].fact else { panic!() };
                match change {
                    "population" => space.population = StoragePopulation::SharedTempdbDataFiles,
                    "negative" => space.total_bytes = Some("-1".into()),
                    _ => space.total_bytes = Some("1.5".into()),
                }
            }
        };
        assert!(!read.is_valid(), "{invalid}");
    }
}

#[test]
fn ambiguous_allocation_parts_and_contradictory_native_file_units_are_refused() {
    let mut read = read();
    let OperationalFact::Space(space) = &mut read.observations[0].fact else { panic!() };
    space.parts = vec![
        SpaceAllocation { kind: SpaceAllocationKind::UserObjects, bytes: Some("0".into()) },
        SpaceAllocation { kind: SpaceAllocationKind::UserObjects, bytes: Some("1".into()) },
    ];
    assert!(!read.is_valid(), "a duplicate dimension cannot become the first representative value");
    read.observations = rows();
    let OperationalFact::MssqlDatabaseFile(file) = &mut read.observations[1].fact else { panic!() };
    file.page_size_bytes = 4096;
    assert!(!read.is_valid(), "the native shape has a documented fixed page unit");
}

#[test]
fn canonical_export_refuses_contradictory_native_file_meanings_before_any_seal_or_projection() {
    let read = read();
    let source = snapshot_source();
    let capture = CaptureIdentity { observation_id: "native-storage-admission".into(), revision: None };
    let original = read.observations[1].clone();
    assert!(ExportedRecord::new(&source, &capture, TelemetryRecord::Operational(original.clone())).is_ok());
    for change in [
        "filestream_allocation",
        "snapshot_allocation",
        "unknown_snapshot",
        "sparse_only_snapshot",
        "offline_used",
        "wrong_kind",
    ] {
        let mut row = original.clone();
        let OperationalFact::MssqlDatabaseFile(file) = &mut row.fact else { panic!() };
        match change {
            "filestream_allocation" => {
                file.native_type = "2".into();
                file.kind = crate::FileKind::Other;
                file.used_pages = None;
            }
            "snapshot_allocation" => file.database_is_snapshot = Some(true),
            "unknown_snapshot" => file.database_is_snapshot = None,
            "sparse_only_snapshot" => {
                file.database_is_snapshot = None;
                file.is_sparse = Some(true);
                file.size_basis = StorageFileSizeBasis::SnapshotMaximum;
            }
            "offline_used" => file.native_state = "6".into(),
            _ => file.kind = crate::FileKind::Log,
        }
        assert!(ExportedRecord::new(&source, &capture, TelemetryRecord::Operational(row)).is_err(), "{change}");
    }
}

#[test]
fn sparse_and_missing_snapshot_flags_remain_independent_and_never_repair_native_size_meanings() {
    let mut row = rows().remove(1);
    let OperationalFact::MssqlDatabaseFile(file) = &mut row.fact else { panic!() };
    file.database_is_snapshot = None;
    file.is_sparse = Some(true);
    file.size_basis = StorageFileSizeBasis::Unknown;
    file.used_pages = None;
    assert!(file.is_valid(), "sparse alone supplies no database snapshot identity");
    file.database_is_snapshot = Some(true);
    file.native_state = "6".into();
    file.is_sparse = Some(false);
    file.size_basis = StorageFileSizeBasis::SnapshotMaximum;
    assert!(file.is_valid(), "offline snapshot filegroups need not have sparse files");
    file.database_is_snapshot = Some(false);
    file.native_type = "2".into();
    file.kind = crate::FileKind::Other;
    file.size_basis = StorageFileSizeBasis::ContainerUsed;
    assert!(file.is_valid());
}

#[test]
fn native_database_file_rows_cannot_acquire_foreign_engine_or_shared_target_authority() {
    let capture = CaptureIdentity { observation_id: "storage-vendor-owner".into(), revision: None };
    for engine in [crate::Engine::Postgres, crate::Engine::Mysql, crate::Engine::Yugabyte] {
        let mut read = read();
        let mut source = snapshot_source();
        read.target = crate::DatabaseIdentity::server(engine, "sample", "fixture.invalid", 1433, None).into();
        source.target = read.target.clone();
        assert!(read.target.is_valid(), "the foreign target is coherent before native-family admission");
        assert!(!read.is_valid(), "{engine:?}");
        assert!(
            ExportedRecord::new(&source, &capture, TelemetryRecord::Operational(rows().remove(1))).is_err(),
            "{engine:?}"
        );
    }
    let mut read = read();
    read.target = CollectionTarget::Shared(SharedResourceIdentity {
        engine: crate::Engine::Mssql,
        kind: SharedResourceKind::Instance,
        namespace: NativeIdentity::new("original-machine"),
        native: NativeIdentity::new("original-instance"),
    });
    assert!(!read.is_valid());
}
