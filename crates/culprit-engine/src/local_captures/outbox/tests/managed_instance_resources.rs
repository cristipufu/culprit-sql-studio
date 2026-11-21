use super::*;
use culprit_collector::analytical::test_support::{ManagedInstanceResourceCase, managed_instance_resource_case};
async fn analyze(
    reader: &OutboxCaptureReader,
    connection: &str,
    id: &str,
) -> Result<Box<LocalManagedInstanceResources>> {
    let guard = OperationGuard::new(reader.timeout()).unwrap();
    let result = reader
        .analyze(LocalCaptureRequest {
            connection,
            observation_id: id,
            analysis: LocalCaptureAnalysisKind::ManagedInstanceResources,
            operation: guard.handle(),
        })
        .await?;
    let LocalCapturedAnalysis::ManagedInstanceResources(result) = result else { panic!() };
    Ok(result)
}
#[tokio::test]
async fn original_local_history_uses_the_existing_canonical_worker_and_native_window() {
    for case in ManagedInstanceResourceCase::ALL {
        let (source, rows) = managed_instance_resource_case(case);
        let fixture = LocalCaptureFixture::with_shutdown(source.clone(), rows.clone(), SourceShutdown::Quiescent).await;
        let reader = OutboxCaptureReader::delivery(
            fixture.outbox.as_ref().unwrap().delivery(),
            CompletedCaptureBudget::default(),
        )
        .unwrap();
        let result = analyze(&reader, "local-evidence", &fixture.manifest.capture.observation_id).await.unwrap();
        assert_eq!(result.read().description, source);
        assert!(result.read().is_valid());
        assert_eq!(result.bytes_read(), fixture.original_bytes());
        assert_eq!(result.read().origin.capture.capture, fixture.manifest.capture);
        assert_eq!(result.report().observed_rows, rows.len() as u32);
        assert_eq!(result.report().observed_at.as_deref(), Some("2020-01-01T06:10:00Z"));
    }
}
#[tokio::test]
async fn local_history_rejects_foreign_connection_small_budget_and_changed_original_files() {
    let (source, rows) = managed_instance_resource_case(ManagedInstanceResourceCase::Complete);
    let mut fixture = LocalCaptureFixture::with_shutdown(source, rows, SourceShutdown::Quiescent).await;
    drop(fixture.outbox.take());
    let reader = OutboxCaptureReader::directory(fixture.directory.clone(), CompletedCaptureBudget::default()).unwrap();
    let id = &fixture.manifest.capture.observation_id;
    assert!(analyze(&reader, "foreign", id).await.is_err());
    assert!(analyze(&reader, "local-evidence", id).await.is_ok());
    let small = OutboxCaptureReader::directory(
        fixture.directory.clone(),
        CompletedCaptureBudget { max_bytes: 1, ..Default::default() },
    )
    .unwrap();
    assert!(analyze(&small, "local-evidence", id).await.is_err());
    let file = fixture.capture_directory.join(&fixture.manifest.files[0].file);
    let original = std::fs::read_to_string(&file).unwrap();
    let changed = original.replacen("9007199254740993", "9007199254740994", 1);
    assert_ne!(changed, original);
    std::fs::write(file, changed).unwrap();
    assert!(analyze(&reader, "local-evidence", id).await.is_err());
}
#[tokio::test]
async fn saved_history_is_independent_idempotent_conflict_checked_and_wholly_withdrawn_when_shared() {
    let (source, rows) = managed_instance_resource_case(ManagedInstanceResourceCase::Complete);
    let fixture = LocalCaptureFixture::with_shutdown(source.clone(), rows, SourceShutdown::Quiescent).await;
    let reader =
        OutboxCaptureReader::delivery(fixture.outbox.as_ref().unwrap().delivery(), CompletedCaptureBudget::default())
            .unwrap();
    let result = analyze(&reader, "local-evidence", &fixture.manifest.capture.observation_id).await.unwrap();
    let read = result.into_read();
    let mut snapshot: culprit_core::Snapshot =
        serde_json::from_str(include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/mssql-demo.json")))
            .unwrap();
    snapshot.server.database = "unrelated-current-db".into();
    crate::scan::CapturedScanEvidence::managed_instance_resources(read.clone()).unwrap().attach(&mut snapshot).unwrap();
    crate::scan::CapturedScanEvidence::managed_instance_resources(read.clone()).unwrap().attach(&mut snapshot).unwrap();
    let analyzed = culprit_checks::analyze(snapshot.clone());
    assert_eq!(analyzed.managed_instance_resources.as_ref().unwrap().observed_rows, 1);
    assert_eq!(
        snapshot.operations.managed_instance_resources.as_deref(),
        Some(&RecordedManagedInstanceResourceRead::Captured { read: read.clone() })
    );
    let mut conflict = read;
    conflict.origin.capture.capture.observation_id = "other-original".into();
    assert!(crate::scan::CapturedScanEvidence::managed_instance_resources(conflict).is_err());
    let shared = culprit_core::redact::redact_snapshot(&snapshot);
    assert!(matches!(
        shared.operations.managed_instance_resources.as_deref(),
        Some(RecordedManagedInstanceResourceRead::Unavailable { .. })
    ));
    let shared_report = culprit_checks::analyze(shared.clone());
    let history = shared_report.managed_instance_resources.unwrap();
    assert!(history.intervals.is_empty());
    assert!(history.observed_at.is_none());
    assert_eq!(history.source_state, culprit_core::SourceState::Unavailable);
}
