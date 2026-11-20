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

fn report(case: FullTextPopulationCase, complete: bool) -> FullTextPopulationReport {
    let (source, rows) = full_text_population_evidence(case);
    let observations = rows
        .iter()
        .map(|row| {
            let TelemetryRecord::Operational(row) = row else { panic!() };
            row
        })
        .collect::<Vec<_>>();
    explain(FullTextPopulationEvidence { source: &source, observations: &observations, read_complete: complete })
        .unwrap()
}
#[test]
fn native_processing_stopped_aborted_and_unknown_states_remain_informational_original_observations() {
    for (case, state) in [
        (FullTextPopulationCase::Processing, 5),
        (FullTextPopulationCase::Stopped, 7),
        (FullTextPopulationCase::Aborted, 11),
        (FullTextPopulationCase::UnknownStatus, 255),
    ] {
        let report = report(case, true);
        assert_eq!(report.entries[0].status, Some(state));
        let original = report.original.as_ref().unwrap();
        assert_eq!(original.target.table.database_id, 7);
        assert_eq!(report.entries[0].observed_at, "2020-01-01T00:00:00.5Z");
        assert!(
            report.entries[0].metrics.iter().any(|metric| metric.text.as_deref() == Some("2019-12-31T18:00:00.007"))
        );
        assert!(report.limitations.contains(&t!("view.full_text_populations.limit.point")));
        let json = serde_json::to_value(report).unwrap();
        assert!(json.get("severity").is_none());
    }
}
#[test]
fn empty_denied_unsupported_null_and_partial_never_become_a_success_or_freshness_verdict() {
    for case in [
        FullTextPopulationCase::Empty,
        FullTextPopulationCase::Denied,
        FullTextPopulationCase::Unsupported,
        FullTextPopulationCase::NativeNull,
        FullTextPopulationCase::Partial,
    ] {
        let output = report(case, true);
        assert!(output.limitations.contains(&t!("view.full_text_populations.limit.point")));
        match case {
            FullTextPopulationCase::Empty => {
                assert!(output.entries.is_empty());
                assert_eq!(output.summary, t!("view.full_text_populations.empty"));
            }
            FullTextPopulationCase::Denied | FullTextPopulationCase::Unsupported => {
                assert_eq!(output.source.state, SourceState::Unavailable);
                assert!(output.entries.is_empty());
                assert!(output.source.reason.is_some());
            }
            FullTextPopulationCase::Partial => {
                assert_eq!(output.source.state, SourceState::Partial);
                assert_eq!(output.summary, t!("view.full_text_populations.partial_summary", count = "1"));
            }
            FullTextPopulationCase::NativeNull => {
                assert_eq!(output.entries[0].status, None);
                assert!(output.entries[0].metrics.iter().all(|metric| {
                    metric.text.as_deref().is_some_and(|text| text.contains(&t!("view.full_text_populations.unknown")))
                }));
            }
            _ => unreachable!(),
        }
    }
}
#[test]
fn incomplete_body_and_duplicate_foreign_or_changed_original_points_do_not_borrow_native_population_proof() {
    let (source, rows) = full_text_population_evidence(FullTextPopulationCase::Processing);
    let TelemetryRecord::Operational(row) = &rows[0] else { panic!() };
    let mut foreign = row.clone();
    let OperationalFact::MssqlFullTextPopulation(point) = &mut foreign.fact else { panic!() };
    point.target.catalog_id += 1;
    foreign.key = point.key().unwrap();
    for observations in [vec![row, row], vec![&foreign]] {
        assert!(
            explain(FullTextPopulationEvidence { source: &source, observations: &observations, read_complete: true })
                .is_err()
        );
    }
    let output =
        explain(FullTextPopulationEvidence { source: &source, observations: &[], read_complete: false }).unwrap();
    assert_eq!(output.source.state, SourceState::Unavailable);
    assert_eq!(output.native_rows, Some(1));
    assert_eq!(output.observed_rows, 0);
}
#[test]
fn sharing_or_invalid_originals_withdraw_context_and_keep_explicit_unavailability_without_a_fake_zero_point() {
    let marker = RecordedOperationalRead::Unavailable {
        reason: culprit_core::message!("view.full_text_populations.sharing_withheld"),
    };
    let report = explain_recorded(&marker).unwrap();
    assert!(report.original.is_none());
    assert!(report.native_rows.is_none());
    assert_eq!(report.source.state, SourceState::Unavailable);
    assert!(report.entries.is_empty());
}
