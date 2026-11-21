use super::*;
use culprit_collector::analytical::test_support::{query_wait_evidence_cases, query_wait_fragment_evidence};
async fn waits(reader: &OutboxCaptureReader, connection: &str, id: &str) -> Result<Box<LocalQueryWaitFragments>> {
    let guard = OperationGuard::new(reader.timeout()).unwrap();
    let result = reader
        .analyze(LocalCaptureRequest {
            connection,
            observation_id: id,
            analysis: LocalCaptureAnalysisKind::QueryWaitFragments,
            operation: guard.handle(),
        })
        .await?;
    let LocalCapturedAnalysis::QueryWaitFragments(result) = result else { panic!("selected native waits") };
    Ok(result)
}
#[tokio::test]
async fn local_wait_history_reuses_one_completed_owner_and_preserves_empty_unknown_disabled_and_fragment_originals() {
    for (source, records) in query_wait_evidence_cases() {
        let fixture = LocalCaptureFixture::query_history(source.clone(), records.clone()).await;
        let reader = OutboxCaptureReader::delivery(
            fixture.outbox.as_ref().unwrap().delivery(),
            CompletedCaptureBudget::default(),
        )
        .unwrap();
        let result = waits(&reader, "local-evidence", &fixture.manifest.capture.observation_id).await.unwrap();
        assert_eq!(result.connection(), "local-evidence");
        assert!(result.read().is_valid());
        assert_eq!(result.read().description, source);
        assert_eq!(
            result.read().rows.len(),
            records.iter().filter(|row| matches!(row, TelemetryRecord::QueryWaits(_))).count()
        );
        assert_eq!(result.bytes_read(), fixture.original_bytes());
        assert_eq!(result.read().read.streams.len(), 1);
        assert_eq!(result.read().read.streams[0].stream, TelemetryStream::QueryWaits);
        assert_eq!(
            serde_json::to_value(result.report()).unwrap(),
            serde_json::to_value(culprit_checks::query_wait_fragments::explain(result.read()).unwrap()).unwrap()
        );
    }
}
#[tokio::test]
async fn local_wait_selection_checks_its_actual_manifest_row_cap_and_refuses_a_foreign_saved_connection() {
    let (source, records) = query_wait_fragment_evidence();
    let fixture = LocalCaptureFixture::query_history(source, records).await;
    let id = &fixture.manifest.capture.observation_id;
    let exact = OutboxCaptureReader::delivery(
        fixture.outbox.as_ref().unwrap().delivery(),
        CompletedCaptureBudget { max_rows: 2, ..Default::default() },
    )
    .unwrap();
    assert_eq!(waits(&exact, "local-evidence", id).await.unwrap().read().rows.len(), 2);
    assert!(waits(&exact, "foreign", id).await.is_err());
    let tight = OutboxCaptureReader::delivery(
        fixture.outbox.as_ref().unwrap().delivery(),
        CompletedCaptureBudget { max_rows: 1, ..Default::default() },
    )
    .unwrap();
    assert!(waits(&tight, "local-evidence", id).await.is_err());
    let guard = OperationGuard::new(exact.timeout()).unwrap();
    let operation = guard.handle();
    drop(guard);
    let result = exact
        .analyze(LocalCaptureRequest {
            connection: "local-evidence",
            observation_id: id,
            analysis: LocalCaptureAnalysisKind::QueryWaitFragments,
            operation,
        })
        .await;
    assert!(result.is_err());
    assert_eq!(exact.readers.available_permits(), 1);
}
