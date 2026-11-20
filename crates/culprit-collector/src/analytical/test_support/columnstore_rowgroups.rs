//! One original selected-current-rowgroup corpus, reused by SDK and both offline destinations.
use super::*;
use culprit_core::{DatabaseIdentity, Engine, ErrorCode};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColumnstoreRowgroupCase {
    States,
    NativeNull,
    Nonclustered,
    ReadOnly,
    Partial,
    Empty,
    Denied,
}
impl ColumnstoreRowgroupCase {
    pub const ALL: [Self; 7] =
        [Self::States, Self::NativeNull, Self::Nonclustered, Self::ReadOnly, Self::Partial, Self::Empty, Self::Denied];
}
pub fn columnstore_rowgroup_evidence(case: ColumnstoreRowgroupCase) -> (CollectionDescription, Vec<TelemetryRecord>) {
    let context = NativeReadContext {
        target: DatabaseIdentity::server(Engine::Mssql, "db", "original.invalid", 1433, None).into(),
        partition: SourcePartition::InstanceLocal {
            member: Some(SharedResourceIdentity {
                engine: Engine::Mssql,
                kind: SharedResourceKind::Instance,
                namespace: NativeIdentity::new("native-machine"),
                native: NativeIdentity::new("native-instance"),
            }),
            epoch: Some("native-process-and-database".into()),
        },
        observed_from: "2020-01-01T00:00:00Z".into(),
        observed_until: "2020-01-01T00:00:01Z".into(),
        database_writable: Some(case != ColumnstoreRowgroupCase::ReadOnly),
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
    let states = if case == ColumnstoreRowgroupCase::States {
        vec![0, 1, 2, 3, 4]
    } else if matches!(case, ColumnstoreRowgroupCase::Empty | ColumnstoreRowgroupCase::Denied) {
        vec![]
    } else {
        vec![3]
    };
    let records = states
        .into_iter()
        .enumerate()
        .map(|(id, state)| {
            let point = MssqlColumnstoreRowgroup {
                context: context.clone(),
                target: target.clone(),
                observed_at: "2020-01-01T00:00:00.5Z".into(),
                partition_number: 1,
                row_group_id: id as u32,
                index_type: if case == ColumnstoreRowgroupCase::Nonclustered { 6 } else { 5 },
                delta_store_hobt_id: None,
                state,
                total_rows: (case != ColumnstoreRowgroupCase::NativeNull).then(|| "2".into()),
                deleted_rows: (case != ColumnstoreRowgroupCase::NativeNull).then(|| "0".into()),
                size_bytes: (case != ColumnstoreRowgroupCase::NativeNull).then(|| "9007199254740993".into()),
            };
            TelemetryRecord::Operational(OperationalObservation {
                source: MSSQL_COLUMNSTORE_ROWGROUPS_SOURCE.into(),
                key: point.key().unwrap(),
                fact: OperationalFact::MssqlColumnstoreRowgroup(Box::new(point)),
            })
        })
        .collect::<Vec<_>>();
    let mut coverage = if case == ColumnstoreRowgroupCase::Denied {
        StateSourceCoverage::unavailable(MSSQL_COLUMNSTORE_ROWGROUPS_SOURCE.into(), ErrorCode::PermissionDenied, None)
    } else {
        StateSourceCoverage::observed(
            MSSQL_COLUMNSTORE_ROWGROUPS_SOURCE.into(),
            records.len() as u32,
            case != ColumnstoreRowgroupCase::Partial,
        )
    };
    if case == ColumnstoreRowgroupCase::Partial {
        coverage.stopped = Some(CollectionStop::RowLimit);
    }
    let source = CollectionDescription {
        target: context.target,
        partition: context.partition,
        source: MSSQL_COLUMNSTORE_ROWGROUPS_SOURCE.into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::ColumnstoreRowgroups {
            target: Box::new(target),
            index_type: if case == ColumnstoreRowgroupCase::Nonclustered { 6 } else { 5 },
            database_writable: context.database_writable.unwrap(),
            snapshot: StateSnapshotDescription {
                dataset: SnapshotDataset::ColumnstoreRowgroups,
                observed_from: context.observed_from,
                observed_until: context.observed_until,
                records: records.len() as u32,
                sources: vec![coverage],
            },
        },
    };
    assert!(source.partition_is_consistent());
    (source, records)
}
