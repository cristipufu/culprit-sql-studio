use super::*;
use culprit_collector::analytical::test_support::{GaleraCase, galera_evidence};
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
async fn original_group_members_and_statistics_are_reanalysed_locally_without_native_credentials_or_provider_work() {
    for case in GaleraCase::ALL {
        let (source, records) = galera_evidence(case);
        let fixture = LocalCaptureFixture::snapshot(source, records).await;
        let outbox = fixture.outbox.as_ref().unwrap();
        let reader = OutboxCaptureReader::delivery(outbox.delivery(), CompletedCaptureBudget::default()).unwrap();
        let analysis = analyze(
            &reader,
            "local-evidence",
            &fixture.manifest.capture.observation_id,
            LocalCaptureAnalysisKind::Galera,
        )
        .await
        .unwrap();
        let LocalSnapshotReport::Galera(report) = analysis.report() else { panic!() };
        assert_eq!(
            serde_json::to_value(report).unwrap(),
            serde_json::to_value(culprit_checks::galera::explain_captured(analysis.original().read()).unwrap())
                .unwrap()
        );
        assert_eq!(
            analysis.original().read().description.target,
            fixture.manifest.description.as_ref().unwrap().target
        );
        assert_eq!(report.sources.len(), 1);
    }
}
#[tokio::test]
async fn wrong_saved_connection_or_analysis_kind_cannot_attach_a_group_read() {
    let (source, records) = galera_evidence(GaleraCase::Queued);
    let fixture = LocalCaptureFixture::snapshot(source, records).await;
    let reader =
        OutboxCaptureReader::delivery(fixture.outbox.as_ref().unwrap().delivery(), CompletedCaptureBudget::default())
            .unwrap();
    let id = &fixture.manifest.capture.observation_id;
    assert!(analyze(&reader, "another-saved-connection", id, LocalCaptureAnalysisKind::Galera).await.is_err());
    assert!(analyze(&reader, "local-evidence", id, LocalCaptureAnalysisKind::TableMemory).await.is_err());
    assert!(analyze(&reader, "local-evidence", id, LocalCaptureAnalysisKind::Galera).await.is_ok());
}
