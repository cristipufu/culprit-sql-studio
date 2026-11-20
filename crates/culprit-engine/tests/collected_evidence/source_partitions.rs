use super::fixture::Fixture;
use culprit_collector::analytical::{
    evidence::EvidenceFamily,
    test_support::{
        assert_operational_capture, replica_activity_evidence_cases, replica_workload_evidence_cases,
        replicated_history_evidence_cases,
    },
};
use culprit_core::{CompletedSpillStatementBinding, telemetry::*};
use culprit_engine::collected_resources;

fn family(records: &[TelemetryRecord]) -> EvidenceFamily {
    match records[0].stream() {
        TelemetryStream::ActivityObservations => EvidenceFamily::ActivityObservations,
        TelemetryStream::OperationalSamples => EvidenceFamily::OperationalSamples,
        TelemetryStream::ObjectDefinitions => EvidenceFamily::ObjectDefinitions,
        _ => panic!("local workload family"),
    }
}

#[tokio::test]
async fn readable_member_workload_replays_exact_local_populations_through_existing_sdk_owners() {
    let cases = replica_workload_evidence_cases();
    assert_eq!(cases.len(), 8);
    let mut original_ids = Vec::new();
    for (index, (source, records)) in cases.into_iter().enumerate() {
        let fixture = Fixture::new(source.clone(), records.clone()).await;
        let opened = fixture.recovery_object_reads();
        let capture = fixture.read_families(&[family(&records)]).await;
        assert_operational_capture(&capture, &source, &records);
        assert_eq!(records.len(), if index < 4 { 1 } else { 2 });
        let context = if source.observation.activity().is_some() {
            let analysis = collected_resources::maintenance(&capture).unwrap();
            assert_eq!(analysis.report.native_rows as usize, records.len());
            assert!(analysis.report.entries.is_empty(), "a held read-only transaction is not maintenance");
            assert_eq!(source.observation.activity().unwrap().database_writable, Some(index < 4));
            analysis.context
        } else if source.observation.snapshot().unwrap().dataset == SnapshotDataset::CachedRuntime {
            let analysis = collected_resources::completed_spills(&capture, None).unwrap();
            assert_eq!(analysis.report.entries.len(), records.len());
            assert!(
                analysis
                    .report
                    .entries
                    .iter()
                    .all(|entry| entry.statement_binding == CompletedSpillStatementBinding::NotRequested)
            );
            analysis.context
        } else {
            let analysis = collected_resources::cached_plans(&capture).unwrap();
            assert_eq!(analysis.report.entries.len(), records.len());
            #[cfg(feature = "mssql")]
            assert!(analysis.report.entries.iter().all(|entry| entry.plan.is_some()));
            analysis.context
        };
        assert_eq!(context.source, source);
        assert_eq!(context.records.len(), records.len());
        for (record, exported) in records.iter().zip(&context.records) {
            assert_eq!(exported.row, ExportedRecord::new(&source, capture.capture(), record.clone()).unwrap());
        }
        let common = capture.records()[0].row().record_id.clone();
        if index < 4 {
            original_ids.push(common);
        } else {
            assert_ne!(
                original_ids[index - 4],
                common,
                "an identical cache handle or session ID belongs to its native member"
            );
        }
        assert_eq!(fixture.recovery_object_reads(), opened, "analysis adds neither recovery reads nor a live source");
    }
}

#[tokio::test]
async fn another_member_cannot_supply_sql_to_an_identical_native_cache_handle() {
    let cases = replica_workload_evidence_cases();
    let (source, rows) = cases[1].clone();
    let runtime = Fixture::with_sequence(source, rows, 910_000).await;
    let runtime = runtime.read_families(&[EvidenceFamily::OperationalSamples]).await;
    let (source, rows) = cases[6].clone();
    let definitions = Fixture::with_sequence(source, rows, 910_001).await;
    let definitions = definitions.read_families(&[EvidenceFamily::ObjectDefinitions]).await;
    let analysis = collected_resources::completed_spills(&runtime, Some(&definitions)).unwrap();
    assert_eq!(analysis.report.entries.len(), 1);
    let entry = &analysis.report.entries[0];
    assert_eq!(entry.statement_binding, CompletedSpillStatementBinding::SourceChanged);
    assert!(entry.statement_preview.is_none() && entry.definition.is_none());
    assert_ne!(analysis.context.source.partition, analysis.definition_context.unwrap().source.partition);
}

#[tokio::test]
async fn role_reset_and_unknown_member_qualifications_survive_while_shared_history_keeps_one_identity() {
    let cases = replica_activity_evidence_cases();
    for (index, (source, rows)) in cases.iter().enumerate() {
        let fixture = Fixture::new(source.clone(), rows.clone()).await;
        let capture = fixture.read_families(&[EvidenceFamily::ActivityObservations]).await;
        let analysis = collected_resources::maintenance(&capture).unwrap();
        assert_eq!(analysis.context.source, *source);
        if index == 1 {
            assert_ne!(source.logical_source_id().unwrap(), cases[0].0.logical_source_id().unwrap());
        } else if index == 2 {
            assert_eq!(source.logical_source_id().unwrap(), cases[0].0.logical_source_id().unwrap());
            assert_ne!(source.captured_range().unwrap().scope_id, cases[0].0.captured_range().unwrap().scope_id);
        } else if index == 3 {
            assert_eq!(source.captured_range().unwrap().scope_id, cases[0].0.captured_range().unwrap().scope_id);
        }
        if !source.is_partition_qualified() {
            assert_eq!(analysis.report.source_state, culprit_core::SourceState::Partial);
            assert!(!source.observation.activity().unwrap().coverage.complete_scope);
        }
    }
    let mut previous = None;
    for (source, rows) in replicated_history_evidence_cases() {
        let fixture = Fixture::new(source.clone(), rows).await;
        let capture = fixture.read().await;
        assert!(capture.is_read_complete());
        assert_eq!(source.partition, SourcePartition::Target);
        let ids = capture.records().iter().map(|row| row.row().record_id.clone()).collect::<Vec<_>>();
        let identity = (source.logical_source_id().unwrap(), source.captured_range().unwrap().scope_id, ids);
        if let Some(previous) = &previous {
            assert_eq!(
                &identity, previous,
                "a read route does not duplicate database-wide Query Store cost identities"
            );
        }
        previous = Some(identity);
    }
}
