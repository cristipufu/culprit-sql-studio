//! One original runtime corpus; SDK, local captures and both stores reuse these same rows and gaps.
use super::*;
use culprit_core::{DatabaseIdentity, Engine, ErrorCode};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FullTextPopulationCase {
    Processing,
    Stopped,
    Aborted,
    UnknownStatus,
    NativeNull,
    Partial,
    Empty,
    Denied,
    Unsupported,
}
impl FullTextPopulationCase {
    pub const ALL: [Self; 9] = [
        Self::Processing,
        Self::Stopped,
        Self::Aborted,
        Self::UnknownStatus,
        Self::NativeNull,
        Self::Partial,
        Self::Empty,
        Self::Denied,
        Self::Unsupported,
    ];
}
pub fn full_text_population_evidence(case: FullTextPopulationCase) -> (CollectionDescription, Vec<TelemetryRecord>) {
    let context = NativeReadContext {
        target: DatabaseIdentity::server(Engine::Mssql, "original-search-db", "original.invalid", 1433, None).into(),
        partition: SourcePartition::InstanceLocal {
            member: Some(SharedResourceIdentity {
                engine: Engine::Mssql,
                kind: SharedResourceKind::Instance,
                namespace: NativeIdentity::new("native-machine"),
                native: NativeIdentity::new("native-member"),
            }),
            epoch: Some("original-process-and-db".into()),
        },
        observed_from: "2020-01-01T00:00:00Z".into(),
        observed_until: "2020-01-01T00:00:01Z".into(),
        database_writable: Some(true),
    };
    let target = MssqlFullTextPopulationTarget {
        table: MssqlObjectIdentity {
            database_id: 7,
            object_id: 100,
            created_without_offset: "2019-01-01T00:00:00.000".into(),
        },
        table_modified_without_offset: "2019-01-01T00:00:00.000".into(),
        catalog_id: 2,
        unique_index_id: 1,
    };
    let empty = matches!(
        case,
        FullTextPopulationCase::Empty | FullTextPopulationCase::Denied | FullTextPopulationCase::Unsupported
    );
    let null = case == FullTextPopulationCase::NativeNull;
    let status = match case {
        FullTextPopulationCase::Stopped => 7,
        FullTextPopulationCase::Aborted => 11,
        FullTextPopulationCase::UnknownStatus => 255,
        _ => 5,
    };
    let rows = if empty {
        vec![]
    } else {
        let point = MssqlFullTextPopulation {
            context: context.clone(),
            target: target.clone(),
            observed_at: "2020-01-01T00:00:00.5Z".into(),
            position: 1,
            population_type: (!null).then_some(1),
            population_type_description: (!null).then(|| "Full population".into()),
            status: (!null).then_some(status),
            status_description: (!null).then(|| format!("native status {status}")),
            completion_type: (!null).then_some(0),
            completion_type_description: None,
            queued_population_type: (!null).then_some(4),
            queued_population_type_description: None,
            clustered_index_scan: (!null).then_some(true),
            range_count: (!null).then_some(4),
            completed_range_count: (!null).then_some(2),
            outstanding_batch_count: (!null).then_some(0),
            started_without_offset: (!null).then(|| "2019-12-31T18:00:00.007".into()),
        };
        vec![TelemetryRecord::Operational(OperationalObservation {
            source: MSSQL_FULL_TEXT_POPULATIONS_SOURCE.into(),
            key: point.key().unwrap(),
            fact: OperationalFact::MssqlFullTextPopulation(Box::new(point)),
        })]
    };
    let coverage = match case {
        FullTextPopulationCase::Denied => StateSourceCoverage::unavailable(
            MSSQL_FULL_TEXT_POPULATIONS_SOURCE.into(),
            ErrorCode::PermissionDenied,
            None,
        ),
        FullTextPopulationCase::Unsupported => {
            StateSourceCoverage::unavailable(MSSQL_FULL_TEXT_POPULATIONS_SOURCE.into(), ErrorCode::Unsupported, None)
        }
        FullTextPopulationCase::Partial => {
            StateSourceCoverage::observed(MSSQL_FULL_TEXT_POPULATIONS_SOURCE.into(), rows.len() as u32, false)
        }
        _ => StateSourceCoverage::observed(MSSQL_FULL_TEXT_POPULATIONS_SOURCE.into(), rows.len() as u32, true),
    };
    let source = CollectionDescription {
        target: context.target,
        partition: context.partition,
        source: MSSQL_FULL_TEXT_POPULATIONS_SOURCE.into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::FullTextPopulations {
            metadata: Box::new(MssqlFullTextPopulationMetadata {
                observed_at: "2020-01-01T00:00:00.5Z".into(),
                schema: Some("dbo".into()),
                table: Some("OriginalDocuments".into()),
            }),
            target: Box::new(target),
            database_writable: true,
            snapshot: StateSnapshotDescription {
                dataset: SnapshotDataset::FullTextPopulations,
                observed_from: context.observed_from,
                observed_until: context.observed_until,
                records: rows.len() as u32,
                sources: vec![coverage],
            },
        },
    };
    assert!(source.partition_is_consistent());
    (source, rows)
}
