use super::*;
use culprit_collector::analytical::test_support::{PoolWaitCounterCase, pool_wait_counter_evidence};
async fn analyze(reader: &OutboxCaptureReader, connection: &str, id: &str) -> Result<Box<LocalWaitCounters>> {
    let guard = OperationGuard::new(reader.timeout()).unwrap();
    let result = reader
        .analyze(LocalCaptureRequest {
            connection,
            observation_id: id,
            analysis: LocalCaptureAnalysisKind::WaitCounters,
            operation: guard.handle(),
        })
        .await?;
    let LocalCapturedAnalysis::WaitCounters(result) = result else { panic!() };
    Ok(result)
}
#[tokio::test]
async fn pool_waits_use_completed_original_snapshot_receipts_and_never_an_operational_repack_or_provider() {
    for case in PoolWaitCounterCase::ALL {
        let (source, rows) = pool_wait_counter_evidence(case);
        let fixture = LocalCaptureFixture::snapshot(source.clone(), rows.clone()).await;
        let reader = OutboxCaptureReader::delivery(
            fixture.outbox.as_ref().unwrap().delivery(),
            CompletedCaptureBudget::default(),
        )
        .unwrap();
        let result = analyze(&reader, "local-evidence", &fixture.manifest.capture.observation_id).await.unwrap();
        assert_eq!(result.read().pool_current.as_deref(), mssql_pool_current_authority(&source).unwrap());
        assert_eq!(result.bytes_read(), fixture.original_bytes());
        assert_eq!(result.collected_from(), fixture.manifest.started_at);
        assert_eq!(Some(result.collected_until()), fixture.manifest.finished_at.as_deref());
        for row in rows {
            let TelemetryRecord::WaitCounter(row) = row else { panic!() };
            let reference = result.reference(&row.source, &row.wait_type).unwrap();
            assert_eq!(reference.stream, TelemetryStream::WaitCounterSamples);
            assert_eq!(reference.capture, fixture.manifest.capture);
        }
        assert!(result.reference("waits", "foreign-wait").is_err());
        assert_eq!(fixture.manifest.source_queries, 0);
    }
}
#[tokio::test]
async fn pool_wait_originals_refuse_foreign_connection_cancellation_limits_and_edited_artifacts() {
    let (source, rows) = pool_wait_counter_evidence(PoolWaitCounterCase::GeneralPurpose);
    let mut fixture = LocalCaptureFixture::snapshot(source, rows).await;
    drop(fixture.outbox.take());
    let reader = OutboxCaptureReader::directory(fixture.directory.clone(), CompletedCaptureBudget::default()).unwrap();
    let id = &fixture.manifest.capture.observation_id;
    assert!(analyze(&reader, "foreign", id).await.is_err());
    assert!(analyze(&reader, "local-evidence", id).await.is_ok());
    let mut budget = CompletedCaptureBudget::default();
    budget.max_bytes = 1;
    let small = OutboxCaptureReader::directory(fixture.directory.clone(), budget).unwrap();
    assert!(analyze(&small, "local-evidence", id).await.is_err());
    let guard = OperationGuard::new(reader.timeout()).unwrap();
    let operation = guard.handle();
    drop(guard);
    assert!(
        reader
            .analyze(LocalCaptureRequest {
                connection: "local-evidence",
                observation_id: id,
                analysis: LocalCaptureAnalysisKind::WaitCounters,
                operation
            })
            .await
            .is_err()
    );
    let file = fixture.capture_directory.join(&fixture.manifest.files[0].file);
    let original = std::fs::read_to_string(&file).unwrap();
    let changed = original.replacen("9007199254740993", "9007199254740994", 1);
    assert_ne!(changed, original);
    std::fs::write(file, changed).unwrap();
    assert!(analyze(&reader, "local-evidence", id).await.is_err());
}
