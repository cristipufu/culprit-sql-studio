use super::*;
fn original() -> (CollectionDescription, OperationalObservation) {
    let context = NativeReadContext {
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
                native: NativeIdentity::new("instance"),
            }),
            epoch: Some("native-process-and-db".into()),
        },
        observed_from: "2020-01-01T00:00:00Z".into(),
        observed_until: "2020-01-01T00:00:01Z".into(),
        database_writable: Some(true),
    };
    let target = MssqlColumnstoreTarget {
        table: MssqlObjectIdentity {
            database_id: 7,
            object_id: 100,
            created_without_offset: "2019-01-01T00:00:00.000".into(),
        },
        index_id: 1,
        table_modified_without_offset: "2019-01-01T00:00:00.000".into(),
        partition_number: Some(1),
    };
    let point = MssqlColumnstoreRowgroup {
        context: context.clone(),
        target: target.clone(),
        observed_at: "2020-01-01T00:00:00.5Z".into(),
        partition_number: 1,
        row_group_id: 0,
        index_type: 5,
        delta_store_hobt_id: None,
        state: 3,
        total_rows: Some("2".into()),
        deleted_rows: Some("0".into()),
        size_bytes: Some("9007199254740993".into()),
    };
    let row = OperationalObservation {
        source: MSSQL_COLUMNSTORE_ROWGROUPS_SOURCE.into(),
        key: point.key().unwrap(),
        fact: OperationalFact::MssqlColumnstoreRowgroup(Box::new(point)),
    };
    let source = CollectionDescription {
        target: context.target,
        partition: context.partition,
        source: MSSQL_COLUMNSTORE_ROWGROUPS_SOURCE.into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::ColumnstoreRowgroups {
            target: Box::new(target),
            index_type: 5,
            database_writable: true,
            snapshot: StateSnapshotDescription {
                dataset: SnapshotDataset::ColumnstoreRowgroups,
                observed_from: context.observed_from,
                observed_until: context.observed_until,
                records: 1,
                sources: vec![StateSourceCoverage::collected(MSSQL_COLUMNSTORE_ROWGROUPS_SOURCE.into(), 1)],
            },
        },
    };
    (source, row)
}
fn report(
    source: &CollectionDescription,
    rows: &[&OperationalObservation],
    complete: bool,
) -> Result<ColumnstoreRowgroupReport> {
    explain(ColumnstoreRowgroupEvidence { source, observations: rows, read_complete: complete })
}
#[test]
fn original_catalog_rows_keep_exact_native_state_quantities_and_configuration_only_guidance() {
    let (source, row) = original();
    let read = report(&source, &[&row], true).unwrap();
    assert_eq!(read.source.state, SourceState::Collected);
    assert_eq!(read.entries[0].state, 3);
    assert_eq!(
        read.entries[0].metrics[0].text.as_deref(),
        Some(t!("view.columnstore_rowgroups.state_value", state = "compressed").as_str())
    );
    assert_eq!(read.entries[0].metrics[3].text.as_deref(), Some("9007199254740993"));
    assert!(read.limitations.contains(&t!("view.columnstore_rowgroups.limit.bytes")));
    assert!(read.limitations.contains(&t!("view.columnstore_rowgroups.limit.deleted")));
    assert_eq!(read.target.index_id, 1);
}
#[test]
fn native_null_is_not_zero_and_a_partial_retained_body_cannot_be_an_empty_health_assessment() {
    let (source, mut row) = original();
    let OperationalFact::MssqlColumnstoreRowgroup(point) = &mut row.fact else { panic!() };
    point.size_bytes = None;
    let read = report(&source, &[&row], true).unwrap();
    assert_eq!(read.entries[0].metrics[3].text.as_deref(), Some(t!("view.columnstore_rowgroups.unknown").as_str()));
    let partial = report(&source, &[], false).unwrap();
    assert_eq!(partial.source.state, SourceState::Unavailable);
    assert_eq!(partial.summary, t!("view.columnstore_rowgroups.unavailable"));
    assert!(partial.entries.is_empty());
    assert!(report(&source, &[], true).is_err());
}
#[test]
fn genuine_empty_and_denied_catalogs_are_distinct_without_index_maintenance_or_health_verdicts() {
    let (mut source, _) = original();
    let SourceObservation::ColumnstoreRowgroups { snapshot, .. } = &mut source.observation else { panic!() };
    snapshot.records = 0;
    snapshot.sources[0] = StateSourceCoverage::collected(MSSQL_COLUMNSTORE_ROWGROUPS_SOURCE.into(), 0);
    let empty = report(&source, &[], true).unwrap();
    assert_eq!(empty.native_rows, 0);
    assert_eq!(empty.summary, t!("view.columnstore_rowgroups.summary", count = "0"));
    assert!(empty.limitations.contains(&t!("view.columnstore_rowgroups.limit.point")));
    let SourceObservation::ColumnstoreRowgroups { snapshot, .. } = &mut source.observation else { panic!() };
    snapshot.sources[0] = StateSourceCoverage::unavailable(
        MSSQL_COLUMNSTORE_ROWGROUPS_SOURCE.into(),
        culprit_core::ErrorCode::PermissionDenied,
        None,
    );
    let denied = report(&source, &[], true).unwrap();
    assert_eq!(denied.source.state, SourceState::Unavailable);
    assert_eq!(denied.summary, t!("view.columnstore_rowgroups.unavailable"));
}
#[test]
fn same_ids_from_another_native_owner_target_kind_or_clock_never_attach() {
    let (source, row) = original();
    for change in ["member", "kind", "target", "clock"] {
        let mut other = source.clone();
        if change == "member" {
            let SourcePartition::InstanceLocal { member: Some(member), .. } = &mut other.partition else { panic!() };
            member.native.id = "foreign".into();
        } else {
            let SourceObservation::ColumnstoreRowgroups { snapshot, target, index_type, .. } = &mut other.observation
            else {
                panic!()
            };
            match change {
                "kind" => *index_type = 6,
                "target" => target.index_id = 2,
                _ => snapshot.observed_until = snapshot.observed_from.clone(),
            }
        }
        assert!(report(&other, &[&row], true).is_err(), "{change}");
    }
}
