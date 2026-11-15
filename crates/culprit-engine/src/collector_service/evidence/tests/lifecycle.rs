use super::*;
use culprit_collector::analytical::evidence::{EvidenceGapReason, RecoveryPayloadBudget};
use culprit_core::{ErrorDetail, SourceState, telemetry::*};
use std::sync::atomic::Ordering;

mod fixture;
use fixture::Fixture;

#[tokio::test]
async fn committed_request_memory_investigation_keeps_participants_and_native_source_qualification() {
    for (source, records) in culprit_collector::analytical::test_support::request_memory_evidence_cases() {
        let expected_state = source.observation.snapshot().unwrap().sources[0].state;
        let fixture = Fixture::capture(source.clone(), records.clone()).await;
        let before = fixture.objects.snapshot();
        let capture = fixture.service.read(&fixture.request, &CollectionCancellation::default()).await.unwrap();
        assert!(capture.is_read_complete(), "{:?}", capture.gaps());
        let analysis = crate::collected_resources::request_memory(&capture).unwrap();
        assert_eq!(analysis.context.source, source);
        assert_eq!(analysis.context.records.len(), records.len());
        assert_eq!(analysis.report.entries.len(), records.len());
        assert_eq!(analysis.report.source_state, Some(expected_state));
        assert_eq!(fixture.objects.snapshot(), before, "investigation does not acquire a writer or change history");
        assert!(fixture.objects.reads.lock().unwrap().iter().all(|key| !key.starts_with("payloads/")));
    }
}

#[tokio::test]
async fn committed_session_memory_investigation_keeps_instance_scope_and_tracking_unknowns() {
    for (source, records) in culprit_collector::analytical::test_support::session_memory_evidence_cases() {
        let expected_state = source.observation.snapshot().unwrap().sources[0].state;
        let fixture = Fixture::capture(source.clone(), records.clone()).await;
        let before = fixture.objects.snapshot();
        let capture = fixture.service.read(&fixture.request, &CollectionCancellation::default()).await.unwrap();
        let analysis = crate::collected_resources::session_memory(&capture).unwrap();
        assert_eq!(analysis.context.source, source);
        assert!(analysis.context.source.target.database().is_none());
        assert_eq!(analysis.report.source_state, Some(expected_state));
        assert_eq!(analysis.report.entries.len(), records.len());
        assert_eq!(before, fixture.objects.snapshot());
        assert!(fixture.objects.reads.lock().unwrap().iter().all(|key| !key.starts_with("payloads/")));
    }
}

#[tokio::test]
async fn committed_page_wait_investigation_reads_only_authenticated_original_waiters_and_targets() {
    for (source, records) in culprit_collector::analytical::test_support::page_wait_evidence_cases() {
        let expected_state = source.observation.snapshot().unwrap().sources[0].state;
        let fixture = Fixture::capture(source.clone(), records.clone()).await;
        let before = fixture.objects.snapshot();
        let capture = fixture.service.read(&fixture.request, &CollectionCancellation::default()).await.unwrap();
        let analysis = crate::collected_resources::page_waits(&capture).unwrap();
        assert_eq!(analysis.context.source, source);
        assert_eq!(analysis.report.source_state, Some(expected_state));
        assert_eq!(analysis.report.entries.len(), records.len());
        assert_eq!(before, fixture.objects.snapshot());
        assert!(fixture.objects.reads.lock().unwrap().iter().all(|key| !key.starts_with("payloads/")));
    }
}

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
    let analysis = crate::collected_evidence::analyze(&capture, None).unwrap();
    assert_eq!(analysis.context.source, source);
    assert_eq!(analysis.context.records.len(), expected_records);
    assert!(!analysis.analyses[0].query_inputs.is_empty());
    assert!(analysis.analyses[0].workload.is_some());
    assert!(
        analysis.analyses[0]
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
async fn explicit_recovery_payload_reads_restore_exact_native_details_and_report_their_cost_without_writes() {
    let (source, records) = culprit_collector::analytical::test_support::object_definition_recovery_only_evidence();
    let original = records[0].clone();
    let mut fixture = Fixture::capture(source.clone(), records).await;
    fixture.request.families = vec![EvidenceFamily::ObjectDefinitions];
    let before = fixture.objects.snapshot();
    let capture = fixture.service.read(&fixture.request, &CollectionCancellation::default()).await.unwrap();
    assert!(capture.records().is_empty());
    assert_eq!(capture.gaps()[0].reason, EvidenceGapReason::DetailInRecovery);
    assert_eq!(capture.metrics().recovery_objects, 0);
    assert_eq!(capture.metrics().recovery_bytes, 0);
    assert!(fixture.objects.reads.lock().unwrap().iter().all(|key| !key.starts_with("payloads/")));

    fixture.request.budget.recovery_payloads = Some(RecoveryPayloadBudget { max_objects: 1, max_bytes: 1 });
    let error = fixture.service.read(&fixture.request, &CollectionCancellation::default()).await.unwrap_err();
    assert_eq!(error.detail, Some(ErrorDetail::Delivery(DeliveryProblem::Capacity)));
    assert!(fixture.objects.reads.lock().unwrap().iter().all(|key| !key.starts_with("payloads/")));

    fixture.objects.allow_payloads.store(true, Ordering::SeqCst);
    fixture.request.budget.recovery_payloads = Some(RecoveryPayloadBudget::default());
    let capture = fixture.service.read(&fixture.request, &CollectionCancellation::default()).await.unwrap();
    assert!(capture.is_read_complete(), "{:?}", capture.gaps());
    assert_eq!(capture.records().len(), 1);
    assert_eq!(capture.records()[0].row(), &ExportedRecord::new(&source, capture.capture(), original).unwrap());
    assert_eq!(capture.metrics().recovery_objects, 1);
    let expected_bytes = before
        .iter()
        .filter(|(key, _)| key.starts_with("payloads/"))
        .map(|(_, (bytes, _))| bytes.len() as u64)
        .sum::<u64>();
    assert_eq!(capture.metrics().recovery_bytes, expected_bytes);
    assert!(expected_bytes > 1024);
    let analysis = crate::collected_resources::cached_plans(&capture).unwrap();
    assert_eq!(
        serde_json::to_value(&analysis.context.metrics).unwrap(),
        serde_json::to_value(capture.metrics()).unwrap()
    );
    assert_eq!(analysis.context.records.len(), 1);
    assert_eq!(fixture.objects.snapshot(), before);
    assert_eq!(*fixture.profiles.0.lock().unwrap(), ["bearer:archive-reader", "bearer:warehouse-reader"]);
}

#[tokio::test]
async fn recovery_opt_in_does_not_replace_missing_rows_or_publication_with_raw_history() {
    let mut fixture = Fixture::new().await;
    fixture.request.budget.recovery_payloads = Some(RecoveryPayloadBudget::default());
    fixture.store.omit_first_row();
    let before = fixture.objects.snapshot();
    let capture = fixture.service.read(&fixture.request, &CollectionCancellation::default()).await.unwrap();
    assert_eq!(capture.records().len(), 1);
    assert_eq!(capture.gaps()[0].reason, EvidenceGapReason::MissingRows);
    assert_eq!(capture.metrics().recovery_objects, 0);
    fixture.store.publication.lock().unwrap().take();
    let capture = fixture.service.read(&fixture.request, &CollectionCancellation::default()).await.unwrap();
    assert!(capture.records().is_empty());
    assert_eq!(capture.gaps()[0].reason, EvidenceGapReason::NotPublished);
    assert_eq!(capture.metrics().recovery_objects, 0);
    assert!(fixture.objects.reads.lock().unwrap().iter().all(|key| !key.starts_with("payloads/")));
    assert_eq!(fixture.objects.snapshot(), before);
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

#[tokio::test]
async fn invalid_recovery_budget_is_rejected_before_archive_or_warehouse_access() {
    let mut fixture = Fixture::new().await;
    fixture.request.budget.recovery_payloads = Some(RecoveryPayloadBudget { max_objects: 0, max_bytes: 1 });
    let before = fixture.objects.snapshot();
    let error = fixture.service.read(&fixture.request, &CollectionCancellation::default()).await.unwrap_err();
    assert_eq!(error.detail, Some(ErrorDetail::Delivery(DeliveryProblem::Capacity)));
    assert!(fixture.objects.reads.lock().unwrap().is_empty());
    assert_eq!(fixture.store.requests.load(Ordering::SeqCst), 0);
    assert_eq!(fixture.objects.snapshot(), before);
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

#[tokio::test(start_paused = true)]
async fn cancellation_and_deadline_drop_the_raw_payload_body_without_changing_history() {
    let (source, records) = culprit_collector::analytical::test_support::object_definition_recovery_only_evidence();
    let mut fixture = Fixture::capture(source, records).await;
    fixture.request.families = vec![EvidenceFamily::ObjectDefinitions];
    fixture.request.budget.recovery_payloads = Some(RecoveryPayloadBudget::default());
    fixture.objects.allow_payloads.store(true, Ordering::SeqCst);
    fixture.objects.stall_payload_body.store(true, Ordering::SeqCst);
    let before = fixture.objects.snapshot();
    fixture.request.budget.timeout_ms = 1;
    let error = fixture.service.read(&fixture.request, &CollectionCancellation::default()).await.unwrap_err();
    assert_eq!(error.detail, Some(ErrorDetail::CollectionStopped(CollectionStop::TimedOut)));
    assert!(fixture.store.requests.load(Ordering::SeqCst) > 0);
    assert_eq!(fixture.objects.active.load(Ordering::SeqCst), 0);
    assert_eq!(fixture.objects.reads.lock().unwrap().iter().filter(|key| key.starts_with("payloads/")).count(), 1);
    fixture.objects.payload_started.notified().await;

    fixture.request.budget.timeout_ms = 1000;
    let cancellation = CollectionCancellation::default();
    let (result, _) = tokio::join!(fixture.service.read(&fixture.request, &cancellation), async {
        fixture.objects.payload_started.notified().await;
        assert_eq!(fixture.objects.active.load(Ordering::SeqCst), 1);
        cancellation.cancel();
    });
    assert_eq!(result.unwrap_err().detail, Some(ErrorDetail::CollectionStopped(CollectionStop::Cancelled)));
    assert_eq!(fixture.objects.active.load(Ordering::SeqCst), 0);
    assert_eq!(fixture.objects.snapshot(), before);
}
