use super::*;
use culprit_collector::analytical::evidence::EvidenceGapReason;
use culprit_core::{ErrorDetail, SourceState, telemetry::*};
use std::sync::atomic::Ordering;

mod fixture;
use fixture::Fixture;

#[tokio::test]
async fn committed_query_capture_reaches_the_existing_offline_analysis_with_exact_native_references() {
    let (source, mut records) = culprit_collector::analytical::test_support::query_evidence();
    for record in &mut records {
        if let TelemetryRecord::QueryRuntime(runtime) = record {
            runtime.executions = "2".into();
        }
    }
    let expected_records = records.len();
    let mut fixture = Fixture::capture(source.clone(), records).await;
    fixture.request.families = vec![
        EvidenceFamily::QueryDefinitions,
        EvidenceFamily::PlanDefinitions,
        EvidenceFamily::QueryRuntime,
        EvidenceFamily::QueryWaits,
    ];
    let before = fixture.objects.snapshot();
    let capture = fixture.service.read(&fixture.request, &CollectionCancellation::default()).await.unwrap();
    assert!(capture.is_read_complete(), "{:?}", capture.gaps());
    let analysis = crate::collected_evidence::analyze(&capture).unwrap();
    assert_eq!(analysis.context.evidence.source, source);
    assert_eq!(analysis.context.evidence.records.len(), expected_records);
    assert!(!analysis.context.query_inputs.is_empty());
    assert!(analysis.workload.is_some());
    assert!(
        analysis
            .limitations
            .iter()
            .any(|limitation| limitation.reason
                == culprit_core::collected_analysis::CollectedAnalysisLimit::MissingSchema)
    );
    assert_eq!(before, fixture.objects.snapshot());
}

#[tokio::test]
async fn archive_read_uses_committed_proofs_and_existing_analysis_without_source_credentials_or_writes() {
    let fixture = Fixture::new().await;
    let before = fixture.objects.snapshot();
    let capture = fixture.service.read(&fixture.request, &CollectionCancellation::default()).await.unwrap();
    assert!(capture.is_read_complete(), "{:?}", capture.gaps());
    assert_eq!(capture.records().len(), 2);
    assert_eq!(capture.metrics().pages, 2);
    let analysis = crate::collected_resources::memory_failures(&capture).unwrap();
    assert_eq!(analysis.report.entries.len(), 2);
    assert!(analysis.context.source.target.database().is_none());
    assert_eq!(
        before,
        fixture.objects.snapshot(),
        "read must not initialize history, acquire an epoch or write proofs"
    );
    assert_eq!(*fixture.profiles.0.lock().unwrap(), ["bearer:archive-reader", "bearer:warehouse-reader"]);
    assert!(fixture.objects.reads.lock().unwrap().iter().all(|key| !key.starts_with("payloads/")));
}

#[tokio::test]
async fn partial_rows_and_missing_projection_proofs_preserve_explicit_read_gaps() {
    let fixture = Fixture::new().await;
    fixture.store.omit_first_row();
    let capture = fixture.service.read(&fixture.request, &CollectionCancellation::default()).await.unwrap();
    assert_eq!(capture.records().len(), 1);
    assert_eq!(capture.gaps()[0].reason, EvidenceGapReason::MissingRows);
    let analysis = crate::collected_resources::memory_failures(&capture).unwrap();
    assert_eq!(analysis.report.source_state, Some(SourceState::Partial));
    let SourceObservation::Snapshot(native) = &analysis.context.source.observation else { panic!() };
    assert_eq!(native.sources[0].state, SourceState::Collected);
    fixture.objects.remove_proofs();
    let service = fixture.runtime.evidence(&fixture.config).unwrap();
    let capture = service.read(&fixture.request, &CollectionCancellation::default()).await.unwrap();
    assert!(capture.records().is_empty());
    assert_eq!(capture.gaps()[0].reason, EvidenceGapReason::ProjectionProofUnavailable);
}

#[tokio::test]
async fn absent_publication_and_permission_failure_cannot_appear_as_healthy_empty_evidence() {
    let fixture = Fixture::new().await;
    fixture.store.publication.lock().unwrap().take();
    let capture = fixture.service.read(&fixture.request, &CollectionCancellation::default()).await.unwrap();
    assert!(capture.records().is_empty());
    assert_eq!(capture.gaps()[0].reason, EvidenceGapReason::NotPublished);
    fixture.store.denied.store(true, Ordering::SeqCst);
    let error = fixture.service.read(&fixture.request, &CollectionCancellation::default()).await.unwrap_err();
    assert_eq!(error.code, culprit_core::ErrorCode::PermissionDenied);
}

#[tokio::test]
async fn missing_remote_history_is_never_initialized_by_investigation() {
    let fixture = Fixture::new().await;
    fixture.objects.remove_history();
    let error = fixture.service.read(&fixture.request, &CollectionCancellation::default()).await.unwrap_err();
    assert_eq!(error.detail, Some(ErrorDetail::Delivery(DeliveryProblem::NotReady)));
    assert!(fixture.objects.snapshot().is_empty());
    assert_eq!(fixture.store.requests.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn unproven_native_rows_and_changed_recovery_manifests_fail_integrity() {
    let fixture = Fixture::new().await;
    fixture.store.corrupt_row();
    let error = fixture.service.read(&fixture.request, &CollectionCancellation::default()).await.unwrap_err();
    assert_eq!(error.detail, Some(ErrorDetail::Delivery(DeliveryProblem::Integrity)));
    fixture.objects.corrupt_manifest();
    fixture.store.requests.store(0, Ordering::SeqCst);
    let error = fixture.service.read(&fixture.request, &CollectionCancellation::default()).await.unwrap_err();
    assert_eq!(error.detail, Some(ErrorDetail::Delivery(DeliveryProblem::Integrity)));
    assert_eq!(fixture.store.requests.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn namespace_binding_and_request_budgets_fail_before_unnecessary_warehouse_reads() {
    let mut fixture = Fixture::new().await;
    fixture.config.shard = "another-shard".into();
    let service = fixture.runtime.evidence(&fixture.config).unwrap();
    let error = service.read(&fixture.request, &CollectionCancellation::default()).await.unwrap_err();
    assert_eq!(error.detail, Some(ErrorDetail::Delivery(DeliveryProblem::DestinationChanged)));
    assert_eq!(fixture.store.requests.load(Ordering::SeqCst), 0);
    fixture.request.budget.max_rows = 1;
    let error = fixture.service.read(&fixture.request, &CollectionCancellation::default()).await.unwrap_err();
    assert_eq!(error.detail, Some(ErrorDetail::Delivery(DeliveryProblem::Capacity)));
    assert_eq!(fixture.store.requests.load(Ordering::SeqCst), 0);
    fixture.request.budget.max_rows = 2;
    fixture.request.budget.max_proof_bytes = 1;
    let error = fixture.service.read(&fixture.request, &CollectionCancellation::default()).await.unwrap_err();
    assert_eq!(error.detail, Some(ErrorDetail::Delivery(DeliveryProblem::Capacity)));
}

#[tokio::test(start_paused = true)]
async fn caller_cancellation_and_deadline_drop_owned_recovery_reads_without_changing_history() {
    let mut fixture = Fixture::new().await;
    let cancellation = CollectionCancellation::default();
    cancellation.cancel();
    let error = fixture.service.read(&fixture.request, &cancellation).await.unwrap_err();
    assert_eq!(error.detail, Some(ErrorDetail::CollectionStopped(CollectionStop::Cancelled)));
    assert!(fixture.objects.reads.lock().unwrap().is_empty());
    fixture.objects.stall.store(true, Ordering::SeqCst);
    fixture.request.budget.timeout_ms = 1;
    let before = fixture.objects.snapshot();
    let error = fixture.service.read(&fixture.request, &CollectionCancellation::default()).await.unwrap_err();
    assert_eq!(error.detail, Some(ErrorDetail::CollectionStopped(CollectionStop::TimedOut)));
    assert_eq!(fixture.objects.active.load(Ordering::SeqCst), 0);
    fixture.request.budget.timeout_ms = 1000;
    let cancellation = CollectionCancellation::default();
    let (result, _) = tokio::join!(fixture.service.read(&fixture.request, &cancellation), async {
        tokio::task::yield_now().await;
        cancellation.cancel();
    });
    assert_eq!(result.unwrap_err().detail, Some(ErrorDetail::CollectionStopped(CollectionStop::Cancelled)));
    assert_eq!(fixture.objects.active.load(Ordering::SeqCst), 0);
    assert_eq!(fixture.objects.snapshot(), before);
}
