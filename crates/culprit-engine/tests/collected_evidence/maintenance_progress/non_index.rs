use super::*;
use std::collections::BTreeSet;

fn non_index(source: &CollectionDescription, rows: &[TelemetryRecord]) -> bool {
    source.target.engine() == culprit_core::Engine::Postgres
        && rows.iter().any(|row| {
            matches!(row,
                TelemetryRecord::Operational(OperationalObservation {
                    fact: OperationalFact::MaintenanceProgress(point), ..
                }) if matches!(point.work,
                    MaintenanceProgressWork::PostgreSqlVacuum { .. }
                        | MaintenanceProgressWork::PostgreSqlAnalyze { .. }
                        | MaintenanceProgressWork::PostgreSqlCluster { .. }
                )
            )
        })
}

#[tokio::test]
async fn native_non_index_layouts_keep_version_availability_exact_counters_and_original_scope() {
    let mut commands = BTreeSet::new();
    let cases = maintenance_progress_evidence_cases().into_iter().filter(|(source, rows)| non_index(source, rows));
    let mut count = 0;
    for (source, rows) in cases {
        count += 1;
        let SourceObservation::MaintenanceProgress { snapshot: header, result } = &source.observation else { panic!() };
        let expected_context = result.read_context(&source, header);
        let fixture = Fixture::new(source.clone(), rows).await;
        let before = fixture.recovery_object_reads();
        let capture = fixture.read_families(&[EvidenceFamily::OperationalSamples]).await;
        let analysis = collected_resources::maintenance_progress(&capture).unwrap();
        assert_eq!(analysis.report.context, expected_context);
        assert_eq!(analysis.context.source, source);
        assert_eq!(analysis.report.binding, FocusedRequestBinding::Matched);
        assert_eq!(analysis.report.native_rows, 1);
        assert_eq!(analysis.report.observed_rows, 1);
        assert_eq!(analysis.report.source_state, SourceState::Partial);
        assert!(analysis.report.read_complete);
        let entry = &analysis.report.entries[0];
        assert_eq!(&entry.actor, result.expected_actor().unwrap());
        commands.insert(entry.command.as_native());
        let has = |key: &str, text: &str| {
            entry.metrics.iter().any(|metric| metric.is(key) && metric.text.as_deref() == Some(text))
        };
        match entry.command {
            MaintenanceCommand::Vacuum => {
                assert!(entry.index.is_none());
                assert!(has("view.maintenance_progress.metric.heap_blocks_scanned", "0"));
                assert!(
                    !entry
                        .metrics
                        .iter()
                        .any(|metric| metric.is("view.maintenance_progress.metric.heap_blocks_vacuumed"))
                );
                let legacy = entry
                    .metrics
                    .iter()
                    .any(|metric| metric.is("view.maintenance_progress.metric.legacy_dead_tuple_capacity"));
                let bytes = entry
                    .metrics
                    .iter()
                    .any(|metric| metric.is("view.maintenance_progress.metric.dead_item_capacity_bytes"));
                assert_ne!(legacy, bytes, "legacy tuple capacity and newer byte capacity are not interchangeable");
            }
            MaintenanceCommand::Analyze => {
                assert!(entry.index.is_none());
                assert!(has("view.maintenance_progress.metric.child_table_oid", "0"));
                assert!(
                    !entry
                        .metrics
                        .iter()
                        .any(|metric| metric.is("view.maintenance_progress.metric.extended_statistics_computed"))
                );
            }
            MaintenanceCommand::Cluster | MaintenanceCommand::VacuumFull => {
                assert!(has("view.maintenance_progress.metric.heap_tuples_scanned", "9007199254740993"));
                assert!(has("view.maintenance_progress.metric.heap_tuples_written", "0"));
                assert_eq!(
                    entry.index.as_ref().map(|index| index.id.as_str()),
                    (entry.command == MaintenanceCommand::Cluster).then_some("16422")
                );
            }
            _ => panic!("native non-index command"),
        }
        assert_eq!(fixture.recovery_object_reads(), before);
        let json = serde_json::to_value(&analysis.report).unwrap();
        for absent in ["percent", "completed_at", "owner_sql", "spill_ms", "resource_bytes"] {
            assert!(json.get(absent).is_none() && json["entries"][0].get(absent).is_none(), "{absent}");
        }
    }
    assert_eq!(count, 8);
    assert_eq!(commands, BTreeSet::from(["VACUUM", "ANALYZE", "CLUSTER", "VACUUM FULL"]));
}

#[tokio::test]
async fn non_index_row_loss_never_rewrites_native_membership_or_reports_command_completion() {
    let mut seen = BTreeSet::new();
    for (source, rows) in
        maintenance_progress_evidence_cases().into_iter().filter(|(source, rows)| non_index(source, rows))
    {
        let TelemetryRecord::Operational(OperationalObservation {
            fact: OperationalFact::MaintenanceProgress(point),
            ..
        }) = &rows[0]
        else {
            panic!()
        };
        if !seen.insert(point.command.as_native()) {
            continue;
        }
        let fixture = Fixture::new(source.clone(), rows).await;
        fixture.remove_first_row(TelemetryStream::OperationalSamples);
        let capture = fixture.read_families(&[EvidenceFamily::OperationalSamples]).await;
        let analysis = collected_resources::maintenance_progress(&capture).unwrap();
        assert!(!analysis.report.read_complete);
        assert_eq!(analysis.report.native_rows, 1);
        assert_eq!(analysis.report.observed_rows, 0);
        assert_eq!(analysis.report.membership, FocusedRequestMembership::Observed);
        assert_eq!(analysis.report.source_state, SourceState::Partial);
        assert!(analysis.report.entries.is_empty());
        assert_eq!(analysis.context.source, source);
        assert!(analysis.context.read_gaps.iter().any(|gap| gap.reason == EvidenceGapReason::MissingRows));
    }
    assert_eq!(seen.len(), 4);
}
