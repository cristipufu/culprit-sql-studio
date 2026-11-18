use super::*;
use culprit_collector::analytical::test_support::{SessionCensusCase, session_census_evidence};

async fn census(reader: &OutboxCaptureReader, id: &str) -> Result<LocalSessionCensus> {
    let guard = OperationGuard::new(reader.timeout()).unwrap();
    let value = reader
        .analyze(LocalCaptureRequest {
            connection: "local-evidence",
            observation_id: id,
            analysis: LocalCaptureAnalysisKind::SessionCensus,
            operation: guard.handle(),
        })
        .await?;
    assert_eq!(value.kind(), LocalCaptureAnalysisKind::SessionCensus);
    let LocalCapturedAnalysis::SessionCensus(value) = value else { panic!("original Activity census") };
    Ok(*value)
}

#[tokio::test]
async fn canonical_single_activity_owner_preserves_every_native_census_case_and_original_receipt() {
    for case in SessionCensusCase::ALL {
        let (source, rows) = session_census_evidence(case);
        let fixture = LocalCaptureFixture::activity(source.clone(), rows).await;
        let reader = OutboxCaptureReader::delivery(
            fixture.outbox.as_ref().unwrap().delivery(),
            CompletedCaptureBudget::default(),
        )
        .unwrap();
        let result = census(&reader, &fixture.manifest.capture.observation_id).await.unwrap();
        let frame = result.original().frame().unwrap();
        assert_eq!(frame.source(), &source);
        assert_eq!(result.original().connection(), "local-evidence");
        assert_eq!(result.original().capture().capture, fixture.manifest.capture);
        assert_eq!(result.original().bytes_read(), fixture.original_bytes());
        assert_eq!(result.report().native_source, frame.native().coverage);
        assert_eq!(
            serde_json::to_value(result.report()).unwrap(),
            serde_json::to_value(culprit_checks::session_census::explain_frame(&frame).unwrap()).unwrap()
        );
        for entry in &result.report().entries {
            assert_eq!(entry.reference.as_ref().unwrap().capture_ref(), *result.original().capture());
        }
    }
}

#[tokio::test]
async fn cold_census_reads_same_capture_once_and_refuses_foreign_journal_connection_budget_cancel_and_changed_body() {
    let (source, rows) = session_census_evidence(SessionCensusCase::Mars);
    let mut fixture = LocalCaptureFixture::activity(source, rows).await;
    let reader =
        OutboxCaptureReader::delivery(fixture.outbox.as_ref().unwrap().delivery(), CompletedCaptureBudget::default())
            .unwrap();
    let original = census(&reader, &fixture.manifest.capture.observation_id).await.unwrap();
    let expected = serde_json::to_value(original.report()).unwrap();
    drop(reader);
    drop(fixture.outbox.take());
    let reader = OutboxCaptureReader::directory(fixture.directory.clone(), CompletedCaptureBudget::default()).unwrap();
    assert_eq!(
        serde_json::to_value(census(&reader, &fixture.manifest.capture.observation_id).await.unwrap().report())
            .unwrap(),
        expected
    );
    for cancelled in [false, true] {
        let guard = OperationGuard::new(reader.timeout()).unwrap();
        let operation = guard.handle();
        if cancelled {
            drop(guard);
        }
        let error = reader
            .analyze(LocalCaptureRequest {
                connection: if cancelled { "local-evidence" } else { "another-connection" },
                observation_id: &fixture.manifest.capture.observation_id,
                analysis: LocalCaptureAnalysisKind::SessionCensus,
                operation,
            })
            .await
            .unwrap_err();
        assert_eq!(
            error.detail,
            Some(if cancelled {
                ErrorDetail::CollectionStopped(CollectionStop::Cancelled)
            } else {
                ErrorDetail::Outbox(OutboxProblem::MissingCapture)
            })
        );
    }
    let small = OutboxCaptureReader::directory(
        fixture.directory.clone(),
        CompletedCaptureBudget { max_rows: 1, ..Default::default() },
    )
    .unwrap();
    assert_eq!(
        census(&small, &fixture.manifest.capture.observation_id).await.unwrap_err().detail,
        Some(ErrorDetail::Delivery(DeliveryProblem::Capacity))
    );
    let body = fixture.capture_directory.join(&fixture.manifest.files[0].file);
    let text = std::fs::read_to_string(&body).unwrap();
    let changed = text.replace("fixture-native-library", "another-native-library");
    assert_ne!(text, changed);
    std::fs::write(body, changed).unwrap();
    assert!(census(&reader, &fixture.manifest.capture.observation_id).await.is_err());
}
