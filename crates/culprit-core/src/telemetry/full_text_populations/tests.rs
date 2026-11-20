use super::*;
use crate::{DatabaseIdentity, Engine, ErrorCode};

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

#[test]
fn original_nullable_runtime_rows_and_availability_survive_the_same_authenticated_record_boundary() {
    for case in FullTextPopulationCase::ALL {
        let (source, rows) = full_text_population_evidence(case);
        assert!(full_text_population_context(&source).is_ok(), "{case:?}");
        for record in rows {
            let encoded = ExportedRecord::new(
                &source,
                &CaptureIdentity { observation_id: "original-fts".into(), revision: None },
                record,
            )
            .unwrap();
            let json = serde_json::to_value(&encoded).unwrap();
            assert!(serde_json::from_value::<ExportedRecord>(json).is_ok());
        }
    }
}
#[test]
fn foreign_source_changed_target_member_clock_and_read_only_rows_never_acquire_original_authority() {
    let (source, rows) = full_text_population_evidence(FullTextPopulationCase::Processing);
    let TelemetryRecord::Operational(row) = &rows[0] else { panic!() };
    let OperationalFact::MssqlFullTextPopulation(original) = &row.fact else { panic!() };
    for case in ["target", "member", "clock", "read-only", "foreign", "source"] {
        let mut point = original.as_ref().clone();
        match case {
            "target" => point.target.catalog_id += 1,
            "member" => {
                let SourcePartition::InstanceLocal { member: Some(member), .. } = &mut point.context.partition else {
                    panic!()
                };
                member.native.id.push_str("-different");
            }
            "clock" => point.observed_at = "2020-01-01T00:00:02Z".into(),
            "read-only" => point.context.database_writable = Some(false),
            "foreign" => {
                point.context.target =
                    DatabaseIdentity::server(Engine::Postgres, "db", "original.invalid", 5432, None).into();
                let SourcePartition::InstanceLocal { member: Some(member), .. } = &mut point.context.partition else {
                    panic!()
                };
                member.engine = Engine::Postgres;
            }
            "source" => {}
            _ => unreachable!(),
        }
        let mut changed = source.clone();
        if case == "source" {
            changed.source = "tables".into();
        }
        assert!(!point.fits_source(&changed), "{case}");
    }
}
#[test]
fn runtime_nulls_are_required_and_unknown_codes_are_not_rewritten_as_known_states() {
    let (_, rows) = full_text_population_evidence(FullTextPopulationCase::NativeNull);
    let TelemetryRecord::Operational(row) = &rows[0] else { panic!() };
    let OperationalFact::MssqlFullTextPopulation(point) = &row.fact else { panic!() };
    let value = serde_json::to_value(point).unwrap();
    for field in [
        "population_type",
        "population_type_description",
        "status",
        "status_description",
        "completion_type",
        "completion_type_description",
        "queued_population_type",
        "queued_population_type_description",
        "clustered_index_scan",
        "range_count",
        "completed_range_count",
        "outstanding_batch_count",
        "started_without_offset",
    ] {
        let mut missing = value.clone();
        missing.as_object_mut().unwrap().remove(field);
        assert!(serde_json::from_value::<MssqlFullTextPopulation>(missing).is_err(), "{field}");
    }
    let mut changed = point.as_ref().clone();
    changed.status = Some(255);
    changed.outstanding_batch_count = Some(0);
    assert!(changed.is_valid());
    assert_eq!(changed.status, Some(255));
    assert_eq!(changed.outstanding_batch_count, Some(0));
}
#[test]
fn a_selected_runtime_recipe_cannot_be_invoked_by_an_unqualified_broad_snapshot_request() {
    let (source, _) = full_text_population_evidence(FullTextPopulationCase::Empty);
    let SourceObservation::FullTextPopulations { target, .. } = &source.observation else { panic!() };
    let request = CollectionRequest {
        subject: CollectionSubject::Database,
        selection: CollectionSelection::FullTextPopulations { target: target.clone() },
        budget: CollectionBudget::default(),
    };
    assert!(request.validate().is_ok());
    assert!(source.matches_request(&request));
    let mut broad = request.clone();
    broad.selection = CollectionSelection::Snapshot { dataset: SnapshotDataset::FullTextPopulations };
    assert!(broad.validate().is_err());
    assert!(
        !RecipeSelector::Collection(TelemetryCollection::Snapshot(SnapshotDataset::FullTextPopulations))
            .matches(&request)
    );
}
