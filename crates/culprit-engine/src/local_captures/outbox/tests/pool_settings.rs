use super::*;
use culprit_collector::analytical::test_support::{PoolSettingsCase, pool_settings_evidence};
async fn analyze(reader: &OutboxCaptureReader, connection: &str, id: &str) -> Result<Box<LocalPoolSettings>> {
    let guard = OperationGuard::new(reader.timeout()).unwrap();
    let result = reader
        .analyze(LocalCaptureRequest {
            connection,
            observation_id: id,
            analysis: LocalCaptureAnalysisKind::PoolSettings,
            operation: guard.handle(),
        })
        .await?;
    let LocalCapturedAnalysis::PoolSettings(result) = result else { panic!("native Settings result") };
    Ok(result)
}
#[tokio::test]
async fn original_pool_settings_replay_locally_with_common_snapshot_receipts_and_no_operational_repacking() {
    for case in PoolSettingsCase::ALL {
        let (source, rows) = pool_settings_evidence(case);
        let fixture = LocalCaptureFixture::snapshot(source.clone(), rows.clone()).await;
        let reader = OutboxCaptureReader::delivery(
            fixture.outbox.as_ref().unwrap().delivery(),
            CompletedCaptureBudget::default(),
        )
        .unwrap();
        let result = analyze(&reader, "local-evidence", &fixture.manifest.capture.observation_id).await.unwrap();
        assert!(result.read().is_valid());
        assert_eq!(result.read().description, source);
        assert_eq!(result.read().origin.capture.capture, fixture.manifest.capture);
        assert_eq!(result.bytes_read(), fixture.original_bytes());
        assert_eq!(result.collected_from(), fixture.manifest.started_at);
        assert_eq!(Some(result.collected_until()), fixture.manifest.finished_at.as_deref());
        assert_eq!(result.read().captured_references().unwrap().len(), rows.len());
        for field in &result.report().fields {
            let reference = result.reference(&field.key).unwrap();
            assert_eq!(reference.stream, TelemetryStream::SettingObservations);
            assert_eq!(reference.capture, fixture.manifest.capture);
        }
        assert_eq!(fixture.manifest.source_queries, 0, "controlled corpus is not native/cost qualification");
    }
}
#[tokio::test]
async fn local_pool_settings_refuse_wrong_connection_cancelled_byte_limited_and_edited_originals() {
    let (source, rows) = pool_settings_evidence(PoolSettingsCase::Exact);
    let mut fixture = LocalCaptureFixture::snapshot(source, rows).await;
    drop(fixture.outbox.take());
    let reader = OutboxCaptureReader::directory(fixture.directory.clone(), CompletedCaptureBudget::default()).unwrap();
    let id = &fixture.manifest.capture.observation_id;
    assert!(analyze(&reader, "foreign-connection", id).await.is_err());
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
                analysis: LocalCaptureAnalysisKind::PoolSettings,
                operation
            })
            .await
            .is_err()
    );
    let file = fixture.capture_directory.join(&fixture.manifest.files[0].file);
    let original = std::fs::read_to_string(&file).unwrap();
    let changed = original.replacen("\"value\":\"0\"", "\"value\":\"1\"", 1);
    assert_ne!(changed, original);
    std::fs::write(file, changed).unwrap();
    assert!(analyze(&reader, "local-evidence", id).await.is_err());
}
#[tokio::test]
async fn independent_saved_pool_original_is_idempotent_never_rebound_to_current_database_and_rejects_replacement() {
    let (source, rows) = pool_settings_evidence(PoolSettingsCase::Exact);
    let fixture = LocalCaptureFixture::snapshot(source.clone(), rows.clone()).await;
    let reader =
        OutboxCaptureReader::delivery(fixture.outbox.as_ref().unwrap().delivery(), CompletedCaptureBudget::default())
            .unwrap();
    let original =
        analyze(&reader, "local-evidence", &fixture.manifest.capture.observation_id).await.unwrap().into_read();
    let mut snapshot: culprit_core::Snapshot =
        serde_json::from_str(include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/mssql-demo.json")))
            .unwrap();
    let current_database = snapshot.server.database.clone();
    crate::scan::CapturedScanEvidence::pool_settings(original.clone()).unwrap().attach(&mut snapshot).unwrap();
    crate::scan::CapturedScanEvidence::pool_settings(original).unwrap().attach(&mut snapshot).unwrap();
    assert_eq!(snapshot.server.database, current_database);
    let Some(RecordedPoolSettingsRead::Captured { read }) = snapshot.operations.pool_settings.as_deref() else {
        panic!()
    };
    assert_eq!(read.description, source);
    let other = LocalCaptureFixture::snapshot(source, rows).await;
    let reader =
        OutboxCaptureReader::delivery(other.outbox.as_ref().unwrap().delivery(), CompletedCaptureBudget::default())
            .unwrap();
    let replacement =
        analyze(&reader, "local-evidence", &other.manifest.capture.observation_id).await.unwrap().into_read();
    assert!(crate::scan::CapturedScanEvidence::pool_settings(replacement).unwrap().attach(&mut snapshot).is_err());
    let report = culprit_checks::analyze(snapshot.clone());
    assert_eq!(report.pool_settings.unwrap().fields.len(), 6);
    let shared = culprit_core::redact::redact_snapshot(&snapshot);
    let report = culprit_checks::analyze(shared);
    let pool = report.pool_settings.unwrap();
    assert_eq!(pool.source_state, culprit_core::SourceState::Unavailable);
    assert!(pool.fields.is_empty() && pool.target.is_none());
}
