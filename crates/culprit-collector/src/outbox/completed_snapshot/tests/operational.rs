use super::*;
use crate::analytical::test_support::{LocalCaptureFixture, MemoryClerkCase, memory_clerk_evidence};
use culprit_core::{SourceState, limits::OperationGuard};

async fn original() -> LocalCaptureFixture {
    let (source, rows) = memory_clerk_evidence(MemoryClerkCase::SharedInstance);
    LocalCaptureFixture::snapshot(source, rows).await
}

async fn read(fixture: &LocalCaptureFixture) -> Result<CompletedSnapshot> {
    let outbox = fixture.outbox.as_ref().unwrap();
    let attempt = outbox.attempt_receipt(&fixture.manifest.capture, None).await?;
    outbox
        .delivery()
        .completed_snapshot_for_connection(
            "local-evidence",
            &attempt,
            SnapshotDataset::Operational,
            CompletedCaptureBudget::default(),
            &CollectionCancellation::default(),
        )
        .await
}

#[tokio::test]
async fn operational_conversion_keeps_all_original_rows_shared_scope_clocks_and_compact_positions() {
    let fixture = original().await;
    assert_eq!(fixture.manifest.status, CollectionStatus::Complete);
    let original = read(&fixture).await.unwrap();
    assert_eq!(original.connection(), "local-evidence");
    assert_eq!(original.collected_from(), fixture.manifest.started_at);
    assert_eq!(Some(original.collected_until()), fixture.manifest.finished_at.as_deref());
    assert_eq!(original.bytes_read(), fixture.original_bytes());
    assert_eq!(original.row_ordinals(), [0, 1, 2]);
    let references = (0..original.records().len()).map(|index| original.reference(index).unwrap()).collect::<Vec<_>>();
    let expected_rows = original.records().iter().map(|row| row.record.clone()).collect::<Vec<_>>();
    let expected_capture = original.capture().clone();
    let retained = original.into_operational_read().unwrap();
    assert!(retained.is_valid());
    assert!(retained.read_complete);
    assert_eq!(retained.origin.capture, expected_capture);
    assert_eq!(retained.description, fixture.manifest.description.clone().unwrap());
    assert_eq!(retained.context().unwrap(), NativeReadContext::from_snapshot(&retained.description).unwrap());
    assert!(matches!(retained.description.target, CollectionTarget::Shared(_)));
    assert_eq!(retained.observations.len(), 3, "unrelated source rows remain part of the original capture");
    assert_eq!(
        serde_json::to_value(
            retained.observations.iter().cloned().map(TelemetryRecord::Operational).collect::<Vec<_>>()
        )
        .unwrap(),
        serde_json::to_value(expected_rows).unwrap()
    );
    assert_eq!(retained.captured_references().unwrap(), references);
    assert_eq!(retained.source_coverage(culprit_core::source_id::MEMORY_CLERKS).unwrap().records, 2);
    let unrelated = retained.source_coverage("independent-native-counters").unwrap();
    assert_eq!(unrelated.records, 1);
    assert_eq!(unrelated.state, SourceState::Collected);
    assert_eq!(
        serde_json::from_slice::<CapturedOperationalRead>(&serde_json::to_vec(&retained).unwrap()).unwrap(),
        retained
    );
}

#[tokio::test]
async fn partial_native_visibility_and_unavailable_sources_survive_a_complete_original_body() {
    for case in [MemoryClerkCase::NativePartial, MemoryClerkCase::Denied] {
        let (source, rows) = memory_clerk_evidence(case);
        let coverage = source.observation.snapshot().unwrap().sources.clone();
        let fixture = LocalCaptureFixture::snapshot(source, rows).await;
        let original = read(&fixture).await.unwrap();
        let retained = original.into_operational_read().unwrap();
        assert!(retained.read_complete);
        assert_eq!(retained.description.observation.snapshot().unwrap().sources, coverage);
        assert_eq!(retained.observations.len(), if case == MemoryClerkCase::Denied { 1 } else { 3 });
    }
}

#[tokio::test]
async fn connection_and_dataset_authority_are_checked_inside_the_same_original_reader() {
    let fixture = original().await;
    let outbox = fixture.outbox.as_ref().unwrap();
    let attempt = outbox.attempt_receipt(&fixture.manifest.capture, None).await.unwrap();
    let delivery = outbox.delivery();
    let error = delivery
        .completed_snapshot_for_connection(
            "foreign-connection",
            &attempt,
            SnapshotDataset::Operational,
            CompletedCaptureBudget::default(),
            &CollectionCancellation::default(),
        )
        .await
        .unwrap_err();
    assert_eq!(error.detail, Some(ErrorDetail::Outbox(OutboxProblem::MissingCapture)));
    let operation = OperationGuard::new(Duration::from_secs(5)).unwrap();
    let error = delivery
        .with_completed_snapshot::<()>(
            CompletedCaptureLookup {
                connection: "local-evidence",
                observation_id: &fixture.manifest.capture.observation_id,
            },
            SnapshotDataset::CachedRuntime,
            CompletedCaptureBudget::default(),
            operation.handle(),
            (),
            |_| panic!("an unsupported header cannot reach projection"),
        )
        .await
        .unwrap_err();
    assert_eq!(error.detail, Some(ErrorDetail::Delivery(DeliveryProblem::UnsupportedSchema)));
    assert_eq!(outbox.status(1, None).await.unwrap().entries[0].state, OutboxCaptureState::Ready);
    assert!(read(&fixture).await.unwrap().into_cached_runtime_read().is_err());
}

#[tokio::test]
async fn conversion_cannot_repair_a_changed_scope_source_or_original_position() {
    let fixture = original().await;
    let mut wrong_scope = read(&fixture).await.unwrap();
    let CollectionTarget::Shared(resource) = &mut wrong_scope.source.target else { panic!() };
    resource.native.id = "different-original-instance".into();
    assert!(wrong_scope.into_operational_read().is_err());

    let mut wrong_source = read(&fixture).await.unwrap();
    wrong_source.source.source = "different-original-source".into();
    assert!(wrong_source.into_operational_read().is_err());

    let mut repeated = read(&fixture).await.unwrap();
    repeated.row_ordinals[1] = repeated.row_ordinals[0];
    assert!(repeated.into_operational_read().is_err());

    let mut outside = read(&fixture).await.unwrap();
    outside.row_ordinals[1] = outside.source.observation.snapshot().unwrap().records;
    assert!(outside.into_operational_read().is_err());
}

#[tokio::test]
async fn operational_lookup_honors_cancellation_and_deadline_before_projecting_provisional_rows() {
    let fixture = original().await;
    let outbox = fixture.outbox.as_ref().unwrap();
    let locked = outbox.files.acquire().await;
    let guard = OperationGuard::new(Duration::from_secs(5)).unwrap();
    let operation = guard.handle();
    drop(guard);
    let error = outbox
        .delivery()
        .with_completed_snapshot::<()>(
            CompletedCaptureLookup {
                connection: "local-evidence",
                observation_id: &fixture.manifest.capture.observation_id,
            },
            SnapshotDataset::Operational,
            CompletedCaptureBudget::default(),
            operation,
            (),
            |_| panic!("cancelled input is never projected"),
        )
        .await
        .unwrap_err();
    assert_eq!(error.detail, Some(ErrorDetail::CollectionStopped(CollectionStop::Cancelled)));

    tokio::time::pause();
    let guard = OperationGuard::new(Duration::from_secs(5)).unwrap();
    let delivery = outbox.delivery();
    let id = fixture.manifest.capture.observation_id.clone();
    let operation = guard.handle();
    let reading = tokio::spawn(async move {
        delivery
            .with_completed_snapshot::<()>(
                CompletedCaptureLookup { connection: "local-evidence", observation_id: &id },
                SnapshotDataset::Operational,
                CompletedCaptureBudget { timeout: Duration::from_millis(1), ..Default::default() },
                operation,
                (),
                |_| panic!("expired input is never projected"),
            )
            .await
    });
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_millis(2)).await;
    assert_eq!(
        reading.await.unwrap().unwrap_err().detail,
        Some(ErrorDetail::CollectionStopped(CollectionStop::TimedOut))
    );
    drop(locked);
    assert!(read(&fixture).await.unwrap().into_operational_read().unwrap().is_valid());
}

#[tokio::test]
async fn cancellation_after_verification_cannot_publish_a_finished_operational_projection() {
    let fixture = original().await;
    let guard = OperationGuard::new(Duration::from_secs(5)).unwrap();
    let operation = guard.handle();
    let error = fixture
        .outbox
        .as_ref()
        .unwrap()
        .delivery()
        .with_completed_snapshot(
            CompletedCaptureLookup {
                connection: "local-evidence",
                observation_id: &fixture.manifest.capture.observation_id,
            },
            SnapshotDataset::Operational,
            CompletedCaptureBudget::default(),
            operation,
            (),
            move |original| {
                assert!(original.into_operational_read()?.is_valid());
                drop(guard);
                Ok(())
            },
        )
        .await
        .unwrap_err();
    assert_eq!(error.detail, Some(ErrorDetail::CollectionStopped(CollectionStop::Cancelled)));
    assert!(read(&fixture).await.unwrap().into_operational_read().unwrap().is_valid());
}
