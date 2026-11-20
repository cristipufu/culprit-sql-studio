//! Exact selected-object memory gauges, reused unchanged by SDK and both offline destinations.
use super::*;
use culprit_core::{DatabaseIdentity, Engine, ErrorCode};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TableMemoryCase {
    Exact,
    Zero,
    NativeNulls,
    MissingRow,
    Denied,
    Unsupported,
    NotApplicable,
    Partial,
    ReadOnly,
}
impl TableMemoryCase {
    pub const ALL: [Self; 9] = [
        Self::Exact,
        Self::Zero,
        Self::NativeNulls,
        Self::MissingRow,
        Self::Denied,
        Self::Unsupported,
        Self::NotApplicable,
        Self::Partial,
        Self::ReadOnly,
    ];
}
pub fn table_memory_evidence(case: TableMemoryCase) -> (CollectionDescription, Vec<TelemetryRecord>) {
    let target = MssqlObjectIdentity {
        database_id: 7,
        object_id: 100,
        created_without_offset: "2019-01-01T00:00:00.0000000".into(),
    };
    let mut metadata = MssqlTableMemoryMetadata {
        observed_at: "2020-01-01T00:00:00.5Z".into(),
        schema: "dbo".into(),
        name: "Hot".into(),
        memory_optimized: Some(true),
        xtp_supported: Some(true),
        dmv_permission: Some(true),
        metadata_visible: Some(true),
        layout_available: true,
    };
    let has_row = !matches!(
        case,
        TableMemoryCase::MissingRow
            | TableMemoryCase::Denied
            | TableMemoryCase::Unsupported
            | TableMemoryCase::NotApplicable
    );
    match case {
        TableMemoryCase::Denied => metadata.dmv_permission = Some(false),
        TableMemoryCase::Unsupported => metadata.layout_available = false,
        TableMemoryCase::NotApplicable => metadata.memory_optimized = Some(false),
        _ => {}
    }
    let records = if has_row {
        let quantity = |value: &str| match case {
            TableMemoryCase::NativeNulls => None,
            TableMemoryCase::Zero => Some("0".into()),
            _ => Some(value.into()),
        };
        let point = MssqlTableMemory {
            table: target.clone(),
            observed_at: metadata.observed_at.clone(),
            allocated_table_kibibytes: quantity("9007199254740993"),
            used_table_kibibytes: quantity("10"),
            allocated_indexes_kibibytes: quantity("0"),
            used_indexes_kibibytes: quantity("4"),
        };
        vec![TelemetryRecord::Operational(OperationalObservation {
            source: MSSQL_TABLE_MEMORY_SOURCE.into(),
            key: point.key().unwrap(),
            fact: OperationalFact::MssqlTableMemory(Box::new(point)),
        })]
    } else {
        vec![]
    };
    let coverage = match case {
        TableMemoryCase::Denied => {
            StateSourceCoverage::unavailable(MSSQL_TABLE_MEMORY_SOURCE.into(), ErrorCode::PermissionDenied, None)
        }
        TableMemoryCase::Unsupported => {
            StateSourceCoverage::unavailable(MSSQL_TABLE_MEMORY_SOURCE.into(), ErrorCode::Unsupported, None)
        }
        TableMemoryCase::NotApplicable => StateSourceCoverage::not_applicable(MSSQL_TABLE_MEMORY_SOURCE.into()),
        _ => {
            let mut coverage = StateSourceCoverage::observed(
                MSSQL_TABLE_MEMORY_SOURCE.into(),
                records.len() as u32,
                case != TableMemoryCase::Partial,
            );
            if case == TableMemoryCase::Partial {
                coverage.stopped = Some(CollectionStop::RowLimit);
            }
            coverage
        }
    };
    let source = CollectionDescription {
        target: DatabaseIdentity::server(Engine::Mssql, "db", "original.invalid", 1433, None).into(),
        partition: SourcePartition::InstanceLocal {
            member: Some(SharedResourceIdentity {
                engine: Engine::Mssql,
                kind: SharedResourceKind::Instance,
                namespace: NativeIdentity::new("private-machine-canary"),
                native: NativeIdentity::new("private-instance-canary"),
            }),
            epoch: Some("private-process-and-database-canary".into()),
        },
        source: MSSQL_TABLE_MEMORY_SOURCE.into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::TableMemory {
            target: Box::new(target),
            metadata: Box::new(metadata),
            database_writable: case != TableMemoryCase::ReadOnly,
            snapshot: StateSnapshotDescription {
                dataset: SnapshotDataset::TableMemory,
                observed_from: "2020-01-01T00:00:00Z".into(),
                observed_until: "2020-01-01T00:00:01Z".into(),
                records: records.len() as u32,
                sources: vec![coverage],
            },
        },
    };
    assert!(source.partition_is_consistent());
    for record in &records {
        assert!(
            ExportedRecord::new(
                &source,
                &CaptureIdentity { observation_id: "original".into(), revision: None },
                record.clone()
            )
            .is_ok()
        );
    }
    (source, records)
}
