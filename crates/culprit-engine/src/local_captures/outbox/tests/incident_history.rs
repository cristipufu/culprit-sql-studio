use super::*;
use culprit_core::{IncidentHistoryProvenance, telemetry::DiagnosticLifecycle};
async fn history(reader: &OutboxCaptureReader, fixture: &LocalCaptureFixture) -> Result<Box<LocalIncidentHistory>> {
    let guard = OperationGuard::new(reader.timeout()).unwrap();
    match reader
        .analyze(LocalCaptureRequest {
            connection: "local-evidence",
            observation_id: &fixture.manifest.capture.observation_id,
            analysis: LocalCaptureAnalysisKind::IncidentHistory,
            operation: guard.handle(),
        })
        .await?
    {
        LocalCapturedAnalysis::IncidentHistory(report) => Ok(report),
        _ => panic!("history selection"),
    }
}
#[tokio::test]
async fn actual_durable_revisions_remain_readable_after_resolved_work_leaves_the_checkpoint() {
    for resolved in [false, true] {
        let fixture = LocalCaptureFixture::diagnostic_history(resolved).await;
        let reader = OutboxCaptureReader::delivery(
            fixture.outbox.as_ref().unwrap().delivery(),
            CompletedCaptureBudget::default(),
        )
        .unwrap();
        let result = history(&reader, &fixture).await.unwrap();
        let report = result.report();
        assert_eq!(report.provenance, IncidentHistoryProvenance::AuthenticatedCapture);
        assert!(report.selected_read_complete);
        assert_eq!(report.entries.len(), 1);
        assert_eq!(
            report.entries[0].lifecycle,
            if resolved { DiagnosticLifecycle::Resolved } else { DiagnosticLifecycle::Active }
        );
        assert_eq!(result.bytes_read(), fixture.original_bytes());
        assert_eq!(report.entries[0].original.as_ref().unwrap().capture, fixture.manifest.capture);
        if resolved {
            assert!(!report.entries[0].predecessor_selected);
            assert!(
                report.entries[0]
                    .evidence
                    .iter()
                    .any(|evidence| evidence.label == culprit_core::t!("view.incident_history.evidence.recovery"))
            );
        }
    }
}
#[tokio::test]
async fn history_keeps_original_connection_tamper_rows_bytes_and_cancellation_admission_strict() {
    let fixture = LocalCaptureFixture::diagnostic_history(true).await;
    let delivery = fixture.outbox.as_ref().unwrap().delivery();
    let reader = OutboxCaptureReader::delivery(delivery.clone(), CompletedCaptureBudget::default()).unwrap();
    let guard = OperationGuard::new(reader.timeout()).unwrap();
    let error = reader
        .analyze(LocalCaptureRequest {
            connection: "foreign",
            observation_id: &fixture.manifest.capture.observation_id,
            analysis: LocalCaptureAnalysisKind::IncidentHistory,
            operation: guard.handle(),
        })
        .await
        .unwrap_err();
    assert_eq!(error.detail, Some(ErrorDetail::Outbox(OutboxProblem::MissingCapture)));
    let guard = OperationGuard::new(reader.timeout()).unwrap();
    let operation = guard.handle();
    drop(guard);
    assert_eq!(
        reader
            .analyze(LocalCaptureRequest {
                connection: "local-evidence",
                observation_id: &fixture.manifest.capture.observation_id,
                analysis: LocalCaptureAnalysisKind::IncidentHistory,
                operation
            })
            .await
            .unwrap_err()
            .detail,
        Some(ErrorDetail::CollectionStopped(CollectionStop::Cancelled))
    );
    let too_small = OutboxCaptureReader::delivery(
        delivery,
        CompletedCaptureBudget { max_bytes: fixture.original_bytes() - 1, ..Default::default() },
    )
    .unwrap();
    assert!(history(&too_small, &fixture).await.is_err());
    let file = fixture.manifest.files.iter().find(|file| file.stream == TelemetryStream::OperationalSamples).unwrap();
    std::fs::write(fixture.capture_directory.join(&file.file), b"private-tamper-canary").unwrap();
    let error = history(&reader, &fixture).await.unwrap_err();
    assert!(!serde_json::to_string(&error).unwrap().contains("private-tamper-canary"));
}
