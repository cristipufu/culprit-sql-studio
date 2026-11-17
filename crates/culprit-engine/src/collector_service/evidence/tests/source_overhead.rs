use super::{super::EvidenceSession, lifecycle::fixture::Fixture};
use culprit_collector::{
    analytical::{
        evidence::{EvidenceFamily, EvidenceGapReason, EvidenceReadBudget},
        test_support,
    },
    capture::CollectionCancellation,
};
use culprit_core::{
    ErrorDetail, t,
    telemetry::{CollectionStatus, CollectionStop, DeliveryProblem, ReadWorkMetrics},
};
use std::{sync::atomic::Ordering, time::Duration};

async fn fixture(metrics: Option<ReadWorkMetrics>) -> Fixture {
    let (source, records) = test_support::memory_failure_evidence();
    Fixture::capture_with_read_metrics(source, records, 100, metrics).await
}
fn payload_reads(fixture: &Fixture) -> usize {
    fixture
        .objects
        .reads
        .lock()
        .unwrap()
        .iter()
        .filter(|key| key.starts_with("payloads/") || key.starts_with("validation/"))
        .count()
}
async fn metadata(
    session: &mut EvidenceSession<'_>,
    fixture: &Fixture,
) -> culprit_collector::analytical::evidence::EvidenceCaptureMetadata {
    session.read_metadata(&fixture.request.range_id).await.unwrap()
}

#[tokio::test]
async fn original_overhead_is_authenticated_without_rows_audit_proofs_or_payloads() {
    let original = test_support::source_overhead_read_metrics();
    let fixture = fixture(Some(original.clone())).await;
    let history = fixture.objects.snapshot();
    fixture.objects.remove_proofs();
    let without_proofs = fixture.objects.snapshot();
    let cancellation = CollectionCancellation::default();
    let mut session = fixture.service.session(fixture.request.budget, &cancellation).unwrap();
    let metadata = metadata(&mut session, &fixture).await;
    assert!(metadata.is_read_complete());
    assert_eq!(metadata.source_queries(), 100);
    assert_eq!(metadata.source_read_metrics(), Some(&original));
    assert_eq!(metadata.metrics().queries, 1);
    assert_eq!(fixture.store.requests.load(Ordering::SeqCst), 1, "only the publication response is requested");
    assert_eq!(metadata.metrics().selected_rows, 0);
    assert_eq!(metadata.metrics().returned_rows, 0);
    assert_eq!(metadata.metrics().pages, 0);
    assert_eq!(metadata.metrics().proof_bytes, 0);
    assert_eq!(metadata.metrics().recovery_objects, 0);
    assert_eq!(payload_reads(&fixture), 0);
    assert_ne!(history, without_proofs);
    assert_eq!(fixture.objects.snapshot(), without_proofs, "inspection cannot repair or rewrite proof objects");
    let analysis = crate::collected_resources::source_overhead(&metadata).unwrap();
    assert_eq!(analysis.report.evidence.status, CollectionStatus::Complete);
    assert_eq!(analysis.report.evidence.source_queries, Some(100));
    assert!(analysis.report.evidence.engine.is_some());
    assert_eq!(analysis.report.evidence.read_metrics.as_ref().unwrap().untracked_tasks, 68);
    assert!(analysis.report.limitations.contains(&t!("view.source_overhead.limit.archive")));
    let json = serde_json::to_string(&analysis).unwrap();
    assert!(!json.contains("overhead_preview_canary"));
    assert!(json.contains("9007199254740993"));
    assert_eq!(analysis.metrics.queries, 1);
    assert_ne!(analysis.metrics.response_bytes.to_string(), original.templates[0].charged_value_bytes);
}

#[tokio::test]
async fn missing_and_recorded_empty_metrics_preserve_the_independent_original_query_total() {
    for metrics in [None, Some(ReadWorkMetrics { active_tasks: 0, untracked_tasks: 0, templates: vec![] })] {
        let fixture = fixture(metrics.clone()).await;
        let cancellation = CollectionCancellation::default();
        let mut session = fixture.service.session(fixture.request.budget, &cancellation).unwrap();
        let metadata = metadata(&mut session, &fixture).await;
        assert_eq!(metadata.source_queries(), 100);
        assert_eq!(metadata.source_read_metrics(), metrics.as_ref());
        let analysis = crate::collected_resources::source_overhead(&metadata).unwrap();
        assert_eq!(analysis.report.evidence.source_queries, Some(100));
        assert_eq!(analysis.report.evidence.read_metrics, metrics);
        assert_eq!(payload_reads(&fixture), 0);
    }
}

#[tokio::test]
async fn absent_publication_is_explicit_without_claiming_native_rows_or_losing_authenticated_metadata() {
    let fixture = fixture(Some(test_support::source_overhead_read_metrics())).await;
    fixture.store.publication.lock().unwrap().take();
    let cancellation = CollectionCancellation::default();
    let mut session = fixture.service.session(fixture.request.budget, &cancellation).unwrap();
    let metadata = metadata(&mut session, &fixture).await;
    assert!(!metadata.is_read_complete());
    assert_eq!(metadata.gaps().len(), 1);
    assert_eq!(metadata.gaps()[0].reason, EvidenceGapReason::NotPublished);
    assert!(metadata.gaps()[0].stream.is_none());
    assert_eq!(metadata.source_queries(), 100);
    assert_eq!(metadata.metrics().queries, 1);
    assert_eq!(payload_reads(&fixture), 0);
    let analysis = crate::collected_resources::source_overhead(&metadata).unwrap();
    assert_eq!(analysis.read_gaps, metadata.gaps());
}

#[tokio::test]
async fn altered_total_or_template_metrics_and_changed_original_manifest_are_integrity_errors() {
    for field in ["SourceQueries", "ReadMetrics", "manifest"] {
        let fixture = fixture(Some(test_support::source_overhead_read_metrics())).await;
        match field {
            "SourceQueries" => fixture.store.replace_publication(field, serde_json::json!(101)),
            "ReadMetrics" => fixture
                .store
                .replace_publication(field, serde_json::json!({"active_tasks":0,"untracked_tasks":0,"templates":[]})),
            "manifest" => fixture.objects.corrupt_manifest(),
            _ => unreachable!(),
        }
        let cancellation = CollectionCancellation::default();
        let mut session = fixture.service.session(fixture.request.budget, &cancellation).unwrap();
        let error = session.read_metadata(&fixture.request.range_id).await.unwrap_err();
        assert_eq!(error.detail, Some(ErrorDetail::Delivery(DeliveryProblem::Integrity)), "{field}: {error:?}");
        let before = fixture.store.requests.load(Ordering::SeqCst);
        assert_eq!(
            session.read_metadata(&fixture.request.range_id).await.unwrap_err().detail,
            Some(ErrorDetail::Delivery(DeliveryProblem::Capacity))
        );
        assert_eq!(fixture.store.requests.load(Ordering::SeqCst), before);
        assert_eq!(payload_reads(&fixture), 0);
    }
}

#[tokio::test]
async fn metadata_and_native_rows_share_one_query_allowance_and_failed_reads_cannot_refill_it() {
    let fixture = fixture(None).await;
    let cancellation = CollectionCancellation::default();
    let mut session = fixture
        .service
        .session(EvidenceReadBudget { max_queries: 1, ..fixture.request.budget }, &cancellation)
        .unwrap();
    metadata(&mut session, &fixture).await;
    let before = fixture.store.requests.load(Ordering::SeqCst);
    assert_eq!(
        session.read(&fixture.request.range_id, &[EvidenceFamily::OperationalSamples]).await.unwrap_err().detail,
        Some(ErrorDetail::Delivery(DeliveryProblem::Capacity))
    );
    assert_eq!(
        session.read_metadata(&fixture.request.range_id).await.unwrap_err().detail,
        Some(ErrorDetail::Delivery(DeliveryProblem::Capacity))
    );
    assert_eq!(fixture.store.requests.load(Ordering::SeqCst), before);
}

#[tokio::test(start_paused = true)]
async fn metadata_uses_the_same_absolute_deadline_and_cancellation_owner() {
    for cancelled in [false, true] {
        let fixture = fixture(None).await;
        let cancellation = CollectionCancellation::default();
        let mut session = fixture
            .service
            .session(EvidenceReadBudget { timeout_ms: 100, ..fixture.request.budget }, &cancellation)
            .unwrap();
        metadata(&mut session, &fixture).await;
        if cancelled {
            cancellation.cancel();
        } else {
            tokio::time::advance(Duration::from_millis(100)).await;
        }
        let before = (fixture.store.requests.load(Ordering::SeqCst), fixture.objects.reads.lock().unwrap().len());
        let error = session.read_metadata(&fixture.request.range_id).await.unwrap_err();
        assert_eq!(
            error.detail,
            Some(ErrorDetail::CollectionStopped(if cancelled {
                CollectionStop::Cancelled
            } else {
                CollectionStop::TimedOut
            }))
        );
        assert_eq!(
            (fixture.store.requests.load(Ordering::SeqCst), fixture.objects.reads.lock().unwrap().len()),
            before
        );
        assert_eq!(fixture.objects.active.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn metadata_does_not_make_an_empty_native_family_read_a_valid_request() {
    let fixture = fixture(None).await;
    let cancellation = CollectionCancellation::default();
    let mut session = fixture.service.session(fixture.request.budget, &cancellation).unwrap();
    let error = session.read(&fixture.request.range_id, &[]).await.unwrap_err();
    assert_eq!(error.detail, Some(ErrorDetail::Delivery(DeliveryProblem::InvalidContract)));
    assert_eq!(fixture.store.requests.load(Ordering::SeqCst), 0);
    assert!(fixture.objects.reads.lock().unwrap().is_empty());
    assert_eq!(
        session.read_metadata(&fixture.request.range_id).await.unwrap_err().detail,
        Some(ErrorDetail::Delivery(DeliveryProblem::Capacity))
    );
}

#[tokio::test]
async fn complete_publication_response_bytes_are_cumulative_across_metadata_reads() {
    let fixture = fixture(Some(test_support::source_overhead_read_metrics())).await;
    let cancellation = CollectionCancellation::default();
    let mut calibration = fixture.service.session(fixture.request.budget, &cancellation).unwrap();
    let original = metadata(&mut calibration, &fixture).await;
    let bytes = original.metrics().response_bytes;
    assert!(bytes > 1024, "the test must exercise the whole publication response rather than a row estimate");
    let budget = EvidenceReadBudget {
        max_bytes: bytes * 2 - 1,
        page_bytes: bytes.try_into().unwrap(),
        ..fixture.request.budget
    };
    let mut session = fixture.service.session(budget, &cancellation).unwrap();
    assert_eq!(metadata(&mut session, &fixture).await.metrics().response_bytes, bytes);
    let error = session.read_metadata(&fixture.request.range_id).await.unwrap_err();
    assert_eq!(error.detail, Some(ErrorDetail::Delivery(DeliveryProblem::Capacity)));
    assert_eq!(
        session.read_metadata(&fixture.request.range_id).await.unwrap_err().detail,
        Some(ErrorDetail::Delivery(DeliveryProblem::Capacity))
    );
    assert_eq!(payload_reads(&fixture), 0);
}

#[tokio::test]
async fn permitting_recovery_payloads_does_not_make_metadata_open_them() {
    let fixture = fixture(Some(test_support::source_overhead_read_metrics())).await;
    let cancellation = CollectionCancellation::default();
    let mut session = fixture
        .service
        .session(
            EvidenceReadBudget { recovery_payloads: Some(Default::default()), ..fixture.request.budget },
            &cancellation,
        )
        .unwrap();
    let metadata = metadata(&mut session, &fixture).await;
    assert_eq!(metadata.metrics().recovery_objects, 0);
    assert_eq!(metadata.metrics().recovery_bytes, 0);
    assert_eq!(payload_reads(&fixture), 0);
}
