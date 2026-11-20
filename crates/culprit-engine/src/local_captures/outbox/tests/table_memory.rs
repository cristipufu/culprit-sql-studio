use super::*;
use culprit_collector::analytical::test_support::{TableMemoryCase, table_memory_evidence};
async fn analyze(
    reader: &OutboxCaptureReader,
    connection: &str,
    id: &str,
    kind: LocalCaptureAnalysisKind,
) -> Result<LocalSnapshotAnalysis> {
    let operation = OperationGuard::new(reader.timeout()).unwrap();
    let result = reader
        .analyze(LocalCaptureRequest { connection, observation_id: id, analysis: kind, operation: operation.handle() })
        .await?;
    let LocalCapturedAnalysis::Snapshot(result) = result else { panic!("canonical selected-table analysis") };
    Ok(result)
}
#[tokio::test]
async fn selected_table_original_reopens_all_native_outcomes_without_new_source_work() {
    for case in TableMemoryCase::ALL {
        let (source, records) = table_memory_evidence(case);
        let SourceObservation::TableMemory { target, .. } = &source.observation else { panic!() };
        let target = target.clone();
        let fixture = LocalCaptureFixture::snapshot(source.clone(), records).await;
        assert_eq!(fixture.manifest.request.selection, CollectionSelection::TableMemory { target: target.clone() });
        let reader = OutboxCaptureReader::delivery(
            fixture.outbox.as_ref().unwrap().delivery(),
            CompletedCaptureBudget::default(),
        )
        .unwrap();
        let analysis = analyze(
            &reader,
            "local-evidence",
            &fixture.manifest.capture.observation_id,
            LocalCaptureAnalysisKind::TableMemory,
        )
        .await
        .unwrap();
        let LocalSnapshotReport::TableMemory(report) = analysis.report() else { panic!() };
        assert_eq!(report.original.as_ref().unwrap().target, *target);
        assert_eq!(analysis.original().read().description, source);
        assert_eq!(
            serde_json::to_value(report).unwrap(),
            serde_json::to_value(culprit_checks::table_memory::explain_captured(analysis.original().read()).unwrap())
                .unwrap()
        );
        assert_eq!(report.source.state, source.observation.snapshot().unwrap().sources[0].state);
        assert_eq!(fixture.manifest.source_queries, 0);
    }
}
#[tokio::test]
async fn wrong_dataset_or_saved_connection_never_relabels_an_original_or_dispatches_a_source() {
    let (source, records) = table_memory_evidence(TableMemoryCase::Exact);
    let fixture = LocalCaptureFixture::snapshot(source, records).await;
    let reader =
        OutboxCaptureReader::delivery(fixture.outbox.as_ref().unwrap().delivery(), CompletedCaptureBudget::default())
            .unwrap();
    let id = &fixture.manifest.capture.observation_id;
    assert!(analyze(&reader, "different-saved-connection", id, LocalCaptureAnalysisKind::TableMemory).await.is_err());
    assert!(analyze(&reader, "local-evidence", id, LocalCaptureAnalysisKind::ColumnstoreRowgroups).await.is_err());
    assert!(analyze(&reader, "local-evidence", id, LocalCaptureAnalysisKind::TableMemory).await.is_ok());
    assert_eq!(fixture.manifest.source_queries, 0);
}
#[tokio::test]
async fn missing_sealed_table_memory_body_fails_fresh_verification_without_a_cached_report_fallback() {
    let (source, records) = table_memory_evidence(TableMemoryCase::Exact);
    let fixture = LocalCaptureFixture::snapshot(source, records).await;
    let reader =
        OutboxCaptureReader::delivery(fixture.outbox.as_ref().unwrap().delivery(), CompletedCaptureBudget::default())
            .unwrap();
    let id = &fixture.manifest.capture.observation_id;
    let original = analyze(&reader, "local-evidence", id, LocalCaptureAnalysisKind::TableMemory).await.unwrap();
    let file = fixture.manifest.files.iter().find(|file| file.stream == TelemetryStream::OperationalSamples).unwrap();
    std::fs::remove_file(fixture.capture_directory.join(&file.file)).unwrap();
    assert!(analyze(&reader, "local-evidence", id, LocalCaptureAnalysisKind::TableMemory).await.is_err());
    assert!(original.original().read().is_valid());
}
