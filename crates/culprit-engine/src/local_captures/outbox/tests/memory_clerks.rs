use super::*;
use culprit_collector::analytical::test_support::{MemoryClerkCase, memory_clerk_evidence};
use culprit_core::{SourceState, source_id};

async fn clerks(reader: &OutboxCaptureReader, id: &str) -> Result<LocalMemoryClerks> {
    let guard = OperationGuard::new(reader.timeout()).unwrap();
    let result = reader
        .analyze(LocalCaptureRequest {
            connection: "local-evidence",
            observation_id: id,
            analysis: LocalCaptureAnalysisKind::MemoryClerks,
            operation: guard.handle(),
        })
        .await?;
    assert_eq!(result.kind(), LocalCaptureAnalysisKind::MemoryClerks);
    let LocalCapturedAnalysis::MemoryClerks(result) = result else { panic!("requested clerk analysis") };
    Ok(result)
}

fn reader(fixture: &LocalCaptureFixture) -> OutboxCaptureReader {
    OutboxCaptureReader::delivery(fixture.outbox.as_ref().unwrap().delivery(), CompletedCaptureBudget::default())
        .unwrap()
}

#[tokio::test]
async fn shared_originals_reach_the_same_view_with_exact_counts_positions_and_independent_clocks() {
    let (source, rows) = memory_clerk_evidence(MemoryClerkCase::SharedInstance);
    let expected_context = NativeReadContext::from_snapshot(&source).unwrap();
    let fixture = LocalCaptureFixture::snapshot(source.clone(), rows).await;
    let result = clerks(&reader(&fixture), &fixture.manifest.capture.observation_id).await.unwrap();
    assert_eq!(result.connection(), "local-evidence");
    assert_eq!(result.bytes_read(), fixture.original_bytes());
    assert_eq!(result.collected_from(), fixture.manifest.started_at);
    assert_eq!(Some(result.collected_until()), fixture.manifest.finished_at.as_deref());
    let raw = result.read();
    assert!(raw.is_valid());
    assert_eq!(raw.description, source);
    assert_eq!(raw.context().unwrap(), expected_context);
    assert!(raw.context().unwrap().database_writable.is_none(), "the read must not invent a role");
    assert_eq!(raw.observations.len(), 3, "the independent source is retained, not filtered from the original body");
    assert_eq!(raw.origin.positions.iter().map(|position| position.row_ordinal).collect::<Vec<_>>(), [0, 1, 2]);
    assert_eq!(raw.origin.capture.capture, fixture.manifest.capture);
    assert_eq!(raw.captured_references().unwrap().len(), 3);
    let report = result.report();
    assert_eq!(report.native_rows, Some(2));
    assert_eq!(report.observed_rows, 2);
    assert_eq!(report.read_complete, Some(true));
    assert_eq!(
        serde_json::to_value(report).unwrap(),
        serde_json::to_value(culprit_checks::memory_clerks::explain_captured(raw).unwrap()).unwrap()
    );
    assert_eq!(report.context.as_ref().unwrap().native(), Some(&expected_context));
    for entry in &report.entries {
        let value = |key: &str| entry.fields.iter().find(|field| field.key == key).unwrap().value.as_deref();
        assert_eq!(value("pages_kb"), Some("9007199254740993"));
        assert_eq!(value("shared_memory_committed_kb"), Some("0"));
        assert_eq!(value("shared_memory_reserved_kb"), None);
    }
    let retained = result.into_read();
    assert_eq!(retained.description, source);
    assert!(retained.is_valid());
}

#[tokio::test]
async fn native_partial_and_denied_sources_do_not_become_healthy_empty_after_a_complete_local_read() {
    for denied in [false, true] {
        let (mut source, mut rows) = memory_clerk_evidence(MemoryClerkCase::SharedInstance);
        let SourceObservation::Snapshot(header) = &mut source.observation else { panic!() };
        if denied {
            rows.retain(
                |row| !matches!(row, TelemetryRecord::Operational(row) if row.source == source_id::MEMORY_CLERKS),
            );
            header.records = rows.len() as u32;
            header.sources[0] = StateSourceCoverage::unavailable(
                source_id::MEMORY_CLERKS.into(),
                culprit_core::ErrorCode::PermissionDenied,
                None,
            );
        } else {
            header.sources[0] = StateSourceCoverage::observed(source_id::MEMORY_CLERKS.into(), 2, false);
        }
        let native_source = header.sources[0].clone();
        let fixture = LocalCaptureFixture::snapshot(source, rows).await;
        let result = clerks(&reader(&fixture), &fixture.manifest.capture.observation_id).await.unwrap();
        assert_eq!(result.report().native_source.as_ref(), Some(&native_source));
        assert_eq!(result.report().read_complete, Some(true));
        assert_eq!(
            result.report().source_state,
            Some(if denied { SourceState::Unavailable } else { SourceState::Partial })
        );
        assert_eq!(result.report().observed_rows, if denied { 0 } else { 2 });
        assert_eq!(result.read().observations.len(), if denied { 1 } else { 3 });
        assert_eq!(result.read().source_coverage("independent-native-counters").unwrap().records, 1);
    }
}

#[tokio::test]
async fn analysis_kind_and_declared_shared_source_cannot_be_inferred_from_a_counter_body() {
    let (source, rows) = memory_clerk_evidence(MemoryClerkCase::SharedInstance);
    let fixture = LocalCaptureFixture::snapshot(source, rows).await;
    let error = read(&reader(&fixture), &fixture.manifest.capture.observation_id).await.unwrap_err();
    assert_eq!(error.detail, Some(ErrorDetail::Delivery(DeliveryProblem::UnsupportedSchema)));

    let case = page_server_read_evidence_cases().remove(0);
    let history = LocalCaptureFixture::query_history(case.source, case.records).await;
    let error = clerks(&reader(&history), &history.manifest.capture.observation_id).await.unwrap_err();
    assert_eq!(error.detail, Some(ErrorDetail::Delivery(DeliveryProblem::UnsupportedSchema)));

    let (database_source, rows) = memory_clerk_evidence(MemoryClerkCase::Exact);
    let database = LocalCaptureFixture::snapshot(database_source, rows).await;
    assert!(clerks(&reader(&database), &database.manifest.capture.observation_id).await.is_err());

    let (mut source, mut rows) = memory_clerk_evidence(MemoryClerkCase::SharedInstance);
    let SourceObservation::Snapshot(header) = &mut source.observation else { panic!() };
    header.sources[0].source = "different-native-source".into();
    for row in &mut rows {
        if let TelemetryRecord::Operational(row) = row
            && row.source == source_id::MEMORY_CLERKS
        {
            row.source = "different-native-source".into();
        }
    }
    let missing = LocalCaptureFixture::snapshot(source, rows).await;
    assert_eq!(missing.manifest.status, CollectionStatus::Complete);
    assert!(clerks(&reader(&missing), &missing.manifest.capture.observation_id).await.is_err());
}

#[tokio::test]
async fn both_analysis_kinds_share_the_original_filesystem_lock_and_saved_connection_admission() {
    let (source, rows) = memory_clerk_evidence(MemoryClerkCase::SharedInstance);
    let mut fixture = LocalCaptureFixture::snapshot(source, rows).await;
    let directory_reader =
        OutboxCaptureReader::directory(fixture.directory.clone(), CompletedCaptureBudget::default()).unwrap();
    let error = clerks(&directory_reader, &fixture.manifest.capture.observation_id).await.unwrap_err();
    assert_eq!(error.detail, Some(ErrorDetail::Outbox(OutboxProblem::Locked)));
    drop(fixture.outbox.take());
    assert_eq!(
        clerks(&directory_reader, &fixture.manifest.capture.observation_id).await.unwrap().report().observed_rows,
        2
    );
    let guard = OperationGuard::new(directory_reader.timeout()).unwrap();
    let error = directory_reader
        .analyze(LocalCaptureRequest {
            connection: "another-connection",
            observation_id: &fixture.manifest.capture.observation_id,
            analysis: LocalCaptureAnalysisKind::MemoryClerks,
            operation: guard.handle(),
        })
        .await
        .unwrap_err();
    assert_eq!(error.detail, Some(ErrorDetail::Outbox(OutboxProblem::MissingCapture)));
}

#[tokio::test]
async fn one_semaphore_preserves_cancelled_and_expired_operations_without_poisoning_later_reads() {
    let (source, rows) = memory_clerk_evidence(MemoryClerkCase::SharedInstance);
    let fixture = LocalCaptureFixture::snapshot(source, rows).await;
    let reader = reader(&fixture);
    let permit = reader.readers.acquire().await.unwrap();
    let guard = OperationGuard::new(reader.timeout()).unwrap();
    let cancelled = guard.handle();
    drop(guard);
    let error = reader
        .analyze(LocalCaptureRequest {
            connection: "local-evidence",
            observation_id: &fixture.manifest.capture.observation_id,
            analysis: LocalCaptureAnalysisKind::MemoryClerks,
            operation: cancelled,
        })
        .await
        .unwrap_err();
    assert_eq!(error.detail, Some(ErrorDetail::CollectionStopped(CollectionStop::Cancelled)));
    tokio::time::pause();
    let guard = OperationGuard::new(Duration::from_millis(1)).unwrap();
    let error = reader
        .analyze(LocalCaptureRequest {
            connection: "local-evidence",
            observation_id: &fixture.manifest.capture.observation_id,
            analysis: LocalCaptureAnalysisKind::MemoryClerks,
            operation: guard.handle(),
        })
        .await
        .unwrap_err();
    assert_eq!(error.detail, Some(ErrorDetail::CollectionStopped(CollectionStop::TimedOut)));
    drop(permit);
    assert!(clerks(&reader, &fixture.manifest.capture.observation_id).await.unwrap().read().is_valid());
}

#[tokio::test]
async fn changed_original_bytes_and_insufficient_budgets_cannot_publish_partial_local_results() {
    let (source, rows) = memory_clerk_evidence(MemoryClerkCase::SharedInstance);
    let fixture = LocalCaptureFixture::snapshot(source, rows).await;
    for budget in [
        CompletedCaptureBudget { max_rows: 2, ..Default::default() },
        CompletedCaptureBudget { max_bytes: fixture.original_bytes() - 1, ..Default::default() },
    ] {
        let reader = OutboxCaptureReader::delivery(fixture.outbox.as_ref().unwrap().delivery(), budget).unwrap();
        let error = clerks(&reader, &fixture.manifest.capture.observation_id).await.unwrap_err();
        assert_eq!(error.detail, Some(ErrorDetail::Delivery(DeliveryProblem::Capacity)));
    }
    assert!(clerks(&reader(&fixture), &fixture.manifest.capture.observation_id).await.is_ok());
    let body = fixture.capture_directory.join(&fixture.manifest.files[0].file);
    let original = std::fs::read_to_string(&body).unwrap();
    let changed = original.replace("9007199254740993", "9007199254740994");
    assert_ne!(original, changed);
    assert_eq!(original.len(), changed.len());
    std::fs::write(body, changed).unwrap();
    assert!(clerks(&reader(&fixture), &fixture.manifest.capture.observation_id).await.is_err());
    assert_eq!(
        fixture.outbox.as_ref().unwrap().status(1, None).await.unwrap().entries[0].state,
        OutboxCaptureState::Quarantined
    );
}

#[tokio::test]
async fn an_unconfirmed_source_owner_cannot_supply_a_clerk_result() {
    let (source, rows) = memory_clerk_evidence(MemoryClerkCase::SharedInstance);
    let fixture = LocalCaptureFixture::with_shutdown(source, rows, SourceShutdown::Unconfirmed).await;
    let error = clerks(&reader(&fixture), &fixture.manifest.capture.observation_id).await.unwrap_err();
    assert_eq!(error.detail, Some(ErrorDetail::Outbox(OutboxProblem::SourceUnconfirmed)));
}
