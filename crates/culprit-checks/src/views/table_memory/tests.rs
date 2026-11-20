use super::*;
fn original() -> (CollectionDescription, OperationalObservation) {
    let target = MssqlObjectIdentity {
        database_id: 7,
        object_id: 100,
        created_without_offset: "2019-01-01T00:00:00.0000000".into(),
    };
    let metadata = MssqlTableMemoryMetadata {
        observed_at: "2020-01-01T00:00:00.5Z".into(),
        schema: "dbo".into(),
        name: "Hot".into(),
        memory_optimized: Some(true),
        xtp_supported: Some(true),
        dmv_permission: Some(true),
        metadata_visible: Some(true),
        layout_available: true,
    };
    let point = MssqlTableMemory {
        table: target.clone(),
        observed_at: metadata.observed_at.clone(),
        allocated_table_kibibytes: Some("9007199254740993".into()),
        used_table_kibibytes: Some("0".into()),
        allocated_indexes_kibibytes: None,
        used_indexes_kibibytes: Some("4".into()),
    };
    let row = OperationalObservation {
        source: MSSQL_TABLE_MEMORY_SOURCE.into(),
        key: point.key().unwrap(),
        fact: OperationalFact::MssqlTableMemory(Box::new(point)),
    };
    let source = CollectionDescription {
        target: culprit_core::DatabaseIdentity::server(
            culprit_core::Engine::Mssql,
            "db",
            "original.invalid",
            1433,
            None,
        )
        .into(),
        partition: SourcePartition::InstanceLocal {
            member: Some(SharedResourceIdentity {
                engine: culprit_core::Engine::Mssql,
                kind: SharedResourceKind::Instance,
                namespace: NativeIdentity::new("machine"),
                native: NativeIdentity::new("member"),
            }),
            epoch: Some("original-process-and-database".into()),
        },
        source: MSSQL_TABLE_MEMORY_SOURCE.into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::TableMemory {
            target: Box::new(target),
            metadata: Box::new(metadata),
            database_writable: true,
            snapshot: StateSnapshotDescription {
                dataset: SnapshotDataset::TableMemory,
                observed_from: "2020-01-01T00:00:00Z".into(),
                observed_until: "2020-01-01T00:00:01Z".into(),
                records: 1,
                sources: vec![StateSourceCoverage::collected(MSSQL_TABLE_MEMORY_SOURCE.into(), 1)],
            },
        },
    };
    (source, row)
}
fn report(
    source: &CollectionDescription,
    rows: &[&OperationalObservation],
    complete: bool,
) -> Result<TableMemoryReport> {
    explain(TableMemoryEvidence { source, observations: rows, read_complete: complete })
}
#[test]
fn four_independent_exact_native_kibibytes_are_not_rows_pressure_or_an_atomic_total() {
    let (source, row) = original();
    let read = report(&source, &[&row], true).unwrap();
    assert_eq!(read.source.state, SourceState::Collected);
    assert_eq!(read.native_rows, Some(1));
    assert_eq!(read.observed_rows, 1);
    assert_eq!(read.summary, t!("view.table_memory.recorded"));
    assert_eq!(read.metrics.len(), 4);
    assert_eq!(read.metrics[0].text.as_deref(), Some(culprit_core::format::digits("9007199254740993".into()).as_str()));
    assert_eq!(read.metrics[1].text.as_deref(), Some("0"));
    assert_eq!(read.metrics[2].text.as_deref(), Some(t!("view.table_memory.unknown").as_str()));
    assert_eq!(read.metrics[3].text.as_deref(), Some("4"));
    assert_eq!(
        read.limitations,
        vec![
            t!("view.table_memory.limit.point"),
            t!("view.table_memory.limit.historical"),
            t!("view.table_memory.limit.versions"),
            t!("view.table_memory.limit.population"),
            t!("view.table_memory.limit.quantities")
        ]
    );
    assert_eq!(read.original.as_ref().unwrap().context.database_writable, Some(true));
    assert_eq!(read.original.as_ref().unwrap().target.object_id, 100);
}
#[test]
fn each_native_null_is_unknown_while_real_zero_is_still_zero() {
    let (source, mut row) = original();
    let OperationalFact::MssqlTableMemory(point) = &mut row.fact else { panic!() };
    point.allocated_table_kibibytes = None;
    point.used_table_kibibytes = None;
    point.used_indexes_kibibytes = None;
    let read = report(&source, &[&row], true).unwrap();
    assert!(read.metrics.iter().all(|metric| metric.text.as_deref() == Some(t!("view.table_memory.unknown").as_str())));
    let OperationalFact::MssqlTableMemory(point) = &mut row.fact else { panic!() };
    point.used_table_kibibytes = Some("0".into());
    assert_eq!(report(&source, &[&row], true).unwrap().metrics[1].text.as_deref(), Some("0"));
}
#[test]
fn unavailable_missing_native_row_disk_table_and_partial_original_are_distinct() {
    let (mut source, _) = original();
    let SourceObservation::TableMemory { snapshot, .. } = &mut source.observation else { panic!() };
    snapshot.records = 0;
    snapshot.sources[0] = StateSourceCoverage::observed(MSSQL_TABLE_MEMORY_SOURCE.into(), 0, true);
    let missing = report(&source, &[], true).unwrap();
    assert_eq!(missing.source.state, SourceState::Empty);
    assert_eq!(missing.summary, t!("view.table_memory.missing_row"));
    assert!(missing.metrics.is_empty());
    for (code, reason) in [
        (culprit_core::ErrorCode::PermissionDenied, t!("view.table_memory.permission")),
        (culprit_core::ErrorCode::Unsupported, t!("view.table_memory.unsupported")),
    ] {
        let SourceObservation::TableMemory { snapshot, metadata, .. } = &mut source.observation else { panic!() };
        metadata.dmv_permission = Some(false);
        snapshot.sources[0] = StateSourceCoverage::unavailable(MSSQL_TABLE_MEMORY_SOURCE.into(), code, None);
        let unavailable = report(&source, &[], true).unwrap();
        assert_eq!(unavailable.source.state, SourceState::Unavailable);
        assert_eq!(unavailable.source.reason, Some(reason));
        assert!(unavailable.metrics.is_empty());
    }
    let SourceObservation::TableMemory { snapshot, metadata, .. } = &mut source.observation else { panic!() };
    metadata.memory_optimized = Some(false);
    snapshot.sources[0] = StateSourceCoverage::not_applicable(MSSQL_TABLE_MEMORY_SOURCE.into());
    assert_eq!(report(&source, &[], true).unwrap().summary, t!("view.table_memory.not_applicable"));
    let (source, row) = original();
    assert!(report(&source, &[], true).is_err());
    let incomplete = report(&source, &[], false).unwrap();
    assert_eq!(incomplete.source.state, SourceState::Unavailable);
    assert_eq!(incomplete.summary, t!("view.table_memory.unavailable"));
    let mut partial = source;
    let SourceObservation::TableMemory { snapshot, .. } = &mut partial.observation else { panic!() };
    snapshot.sources[0] = StateSourceCoverage::observed(MSSQL_TABLE_MEMORY_SOURCE.into(), 1, false);
    snapshot.sources[0].stopped = Some(CollectionStop::RowLimit);
    let read = report(&partial, &[&row], true).unwrap();
    assert_eq!(read.source.state, SourceState::Partial);
    assert_eq!(read.metrics.len(), 4);
    assert!(read.limitations.contains(&t!("view.table_memory.partial")));
}
#[test]
fn wrong_selected_object_generation_clock_source_family_and_duplicate_rows_never_attach() {
    let (source, row) = original();
    for change in ["object", "creation", "clock", "permission", "family", "scope"] {
        let mut altered = source.clone();
        if change == "scope" {
            altered.partition = SourcePartition::Target;
        } else {
            let SourceObservation::TableMemory { target, snapshot, metadata, .. } = &mut altered.observation else {
                panic!()
            };
            match change {
                "object" => target.object_id += 1,
                "creation" => target.created_without_offset = "2020-01-01T00:00:00.0000000".into(),
                "clock" => snapshot.observed_until = snapshot.observed_from.clone(),
                "permission" => metadata.dmv_permission = Some(false),
                _ => snapshot.dataset = SnapshotDataset::Indexes,
            }
        }
        assert!(report(&altered, &[&row], true).is_err(), "{change}");
    }
    assert!(report(&source, &[&row, &row], true).is_err());
}
