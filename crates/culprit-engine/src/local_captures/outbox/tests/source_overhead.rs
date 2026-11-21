use super::*;
use culprit_collector::analytical::test_support::{SourceAcquisitionCase, assert_native_source_overhead_summary};
use culprit_core::{HistoricalSourceOverhead, SourceAcquisitionSeal};
async fn analyze(reader: &OutboxCaptureReader, connection: &str, id: &str) -> Result<Box<LocalSourceOverhead>> {
    let guard = OperationGuard::new(reader.timeout()).unwrap();
    let result = reader
        .analyze(LocalCaptureRequest {
            connection,
            observation_id: id,
            analysis: LocalCaptureAnalysisKind::SourceOverhead,
            operation: guard.handle(),
        })
        .await?;
    let LocalCapturedAnalysis::SourceOverhead(result) = result else { panic!() };
    Ok(result)
}
#[tokio::test]
async fn successful_failed_and_unconfirmed_original_metadata_reuses_one_interpreter_without_payloads_or_identity_borrowing()
 {
    for case in [SourceAcquisitionCase::Complete, SourceAcquisitionCase::Failed, SourceAcquisitionCase::Unconfirmed] {
        let fixture = LocalCaptureFixture::source_acquisition(case).await;
        let delivery = fixture.outbox.as_ref().unwrap().delivery();
        let before = fixture.outbox.as_ref().unwrap().status(10, None).await.unwrap();
        let reader = OutboxCaptureReader::delivery(delivery, Default::default()).unwrap();
        let analysis = analyze(&reader, "local-evidence", &fixture.manifest.capture.observation_id).await.unwrap();
        assert_eq!(analysis.connection(), "local-evidence");
        let original = analysis.original().unwrap();
        assert!(original.is_saveable());
        assert_eq!(original.seal, SourceAcquisitionSeal::Sealed);
        assert_eq!(original.summary.capture, fixture.manifest.capture);
        assert_eq!(original.summary.source_queries, Some(100));
        if case != SourceAcquisitionCase::Complete {
            assert!(original.summary.engine.is_none());
        }
        let report = analysis.measurements().unwrap();
        assert_eq!(
            serde_json::to_value(report).unwrap(),
            serde_json::to_value(culprit_checks::source_overhead::explain_recorded(original).unwrap()).unwrap()
        );
        assert_native_source_overhead_summary(
            &serde_json::to_value(original.summary.read_metrics.as_ref().unwrap()).unwrap(),
            fixture.manifest.read_metrics.as_ref().unwrap(),
        );
        assert_eq!(
            analysis.bytes_read(),
            std::fs::metadata(fixture.capture_directory.join("manifest.json")).unwrap().len()
        );
        let after = fixture.outbox.as_ref().unwrap().status(10, None).await.unwrap();
        assert_eq!((after.ready, after.source_debts), (before.ready, before.source_debts));
    }
}
#[tokio::test]
async fn read_only_directory_inspection_preserves_active_owner_foreign_route_refusal_and_exact_manifest_cap() {
    let fixture = LocalCaptureFixture::source_acquisition(SourceAcquisitionCase::Failed).await;
    let bytes = std::fs::metadata(fixture.capture_directory.join("manifest.json")).unwrap().len();
    let reader = OutboxCaptureReader::directory(
        fixture.directory.clone(),
        CompletedCaptureBudget { max_bytes: bytes, ..Default::default() },
    )
    .unwrap();
    let id = &fixture.manifest.capture.observation_id;
    assert!(analyze(&reader, "foreign", id).await.is_err());
    assert_eq!(analyze(&reader, "local-evidence", id).await.unwrap().bytes_read(), bytes);
    assert_eq!(
        Outbox::open(&fixture.directory).err().unwrap().detail,
        Some(ErrorDetail::Outbox(OutboxProblem::Locked))
    );
    let tight = OutboxCaptureReader::directory(
        fixture.directory.clone(),
        CompletedCaptureBudget { max_bytes: bytes - 1, ..Default::default() },
    )
    .unwrap();
    assert!(analyze(&tight, "local-evidence", id).await.is_err());
    assert_eq!(tight.readers.available_permits(), 1);
}
#[tokio::test]
async fn allocation_without_manifest_is_typed_unavailable_not_a_borrowed_clock_or_zero_work() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("outbox");
    let mut outbox = Outbox::create(&path, Default::default()).unwrap();
    let request = CollectionRequest {
        subject: CollectionSubject::Database,
        selection: CollectionSelection::Activity,
        budget: Default::default(),
    };
    let lease = outbox.reserve("local-evidence", &request).await.unwrap();
    let id = lease.identity().observation_id.clone();
    let reader = OutboxCaptureReader::directory(path, Default::default()).unwrap();
    let result = analyze(&reader, "local-evidence", &id).await.unwrap();
    assert!(result.original().is_none());
    assert!(matches!(result.report(),HistoricalSourceOverhead::Unavailable{reason}
        if reason==&culprit_core::t!("view.source_overhead.initial_missing")));
    assert!(result.measurements().is_none());
    lease.finish().await.unwrap();
}
#[tokio::test]
async fn cancellation_at_projection_withdraws_publication_and_keeps_the_same_gate_until_worker_settlement() {
    let fixture = LocalCaptureFixture::source_acquisition(SourceAcquisitionCase::Failed).await;
    let mut reader = OutboxCaptureReader::directory(fixture.directory.clone(), Default::default()).unwrap();
    let guard = OperationGuard::new(reader.timeout()).unwrap();
    let operation = guard.handle();
    let retained = std::sync::Mutex::new(Some(guard));
    reader.projection_boundary = Some(Arc::new(move || {
        drop(retained.lock().unwrap().take());
        Ok(())
    }));
    let result = reader
        .analyze(LocalCaptureRequest {
            connection: "local-evidence",
            observation_id: &fixture.manifest.capture.observation_id,
            analysis: LocalCaptureAnalysisKind::SourceOverhead,
            operation,
        })
        .await;
    assert_eq!(result.unwrap_err().detail, Some(ErrorDetail::CollectionStopped(CollectionStop::Cancelled)));
    assert_eq!(reader.readers.available_permits(), 1);
    let status = fixture.outbox.as_ref().unwrap().status(10, None).await.unwrap();
    assert_eq!(status.ready, 0);
}
