use super::{super::EvidenceSession, lifecycle::fixture::Fixture};
use culprit_collector::{
    analytical::evidence::{EvidenceFamily, EvidenceGapReason, EvidenceReadBudget, RecoveryPayloadBudget},
    capture::CollectionCancellation,
};
use culprit_core::{
    ErrorDetail,
    telemetry::{CollectionStop, DeliveryProblem},
};
use std::{sync::atomic::Ordering, time::Duration};
use tokio::time::Instant;

fn io_counts(fixture: &Fixture) -> (usize, usize) {
    (fixture.objects.reads.lock().unwrap().len(), fixture.store.requests.load(Ordering::SeqCst))
}

fn payload_reads(fixture: &Fixture) -> usize {
    fixture.objects.reads.lock().unwrap().iter().filter(|key| key.starts_with("payloads/")).count()
}

async fn assert_poisoned(session: &mut EvidenceSession<'_>, fixture: &Fixture) {
    let before = io_counts(fixture);
    let error = session.read(&fixture.request.range_id, &fixture.request.families).await.unwrap_err();
    assert_eq!(error.detail, Some(ErrorDetail::Delivery(DeliveryProblem::Capacity)));
    assert_eq!(io_counts(fixture), before, "a closed action cannot open recovery or warehouse work again");
    assert_eq!(fixture.objects.active.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn selected_manifest_rows_consume_the_action_budget_even_when_no_verified_rows_return() {
    for unpublished in [false, true] {
        let fixture = Fixture::new().await;
        if unpublished {
            fixture.store.publication.lock().unwrap().take();
        } else {
            fixture.store.omit_first_row();
        }
        let cancellation = CollectionCancellation::default();
        let mut session = fixture
            .service
            .session(EvidenceReadBudget { max_rows: 3, ..fixture.request.budget }, &cancellation)
            .unwrap();
        let capture = session.read(&fixture.request.range_id, &fixture.request.families).await.unwrap();
        assert_eq!(capture.metrics().selected_rows, 2);
        assert_eq!(capture.metrics().returned_rows, if unpublished { 0 } else { 1 });
        assert_eq!(capture.records().len(), if unpublished { 0 } else { 1 });
        assert_eq!(
            capture.gaps()[0].reason,
            if unpublished { EvidenceGapReason::NotPublished } else { EvidenceGapReason::MissingRows }
        );
        let warehouse_before = fixture.store.requests.load(Ordering::SeqCst);
        let error = session.read(&fixture.request.range_id, &fixture.request.families).await.unwrap_err();
        assert_eq!(error.detail, Some(ErrorDetail::Delivery(DeliveryProblem::Capacity)));
        assert_eq!(
            fixture.store.requests.load(Ordering::SeqCst),
            warehouse_before,
            "remaining rows are checked before warehouse access"
        );
        assert_poisoned(&mut session, &fixture).await;
    }
}

#[tokio::test]
async fn distinct_selected_families_can_complete_in_one_action_without_charging_unselected_rows() {
    let (source, records) = culprit_collector::analytical::test_support::query_evidence();
    let families = [EvidenceFamily::QueryDefinitions, EvidenceFamily::PlanDefinitions];
    let expected =
        families.map(|family| records.iter().filter(|record| record.stream() == family.stream()).count() as u32);
    assert!(expected.iter().all(|count| *count > 0));
    let selected_rows = expected.iter().sum();
    assert!((selected_rows as usize) < records.len(), "runtime and waits remain outside this selection");
    let fixture = Fixture::capture(source, records).await;
    let cancellation = CollectionCancellation::default();
    let mut session = fixture
        .service
        .session(EvidenceReadBudget { max_rows: selected_rows, ..fixture.request.budget }, &cancellation)
        .unwrap();
    for (family, rows) in families.into_iter().zip(expected) {
        let capture = session.read(&fixture.request.range_id, &[family]).await.unwrap();
        assert!(capture.is_read_complete(), "{:?}", capture.gaps());
        assert_eq!(capture.metrics().selected_rows, rows);
        assert_eq!(capture.records().len(), rows as usize);
        assert!(capture.records().iter().all(|record| record.row().record.stream() == family.stream()));
    }
    assert_poisoned(&mut session, &fixture).await;
}

#[tokio::test]
async fn warehouse_queries_complete_responses_and_projection_proofs_share_one_action_allowance() {
    let fixture = Fixture::new().await;
    let calibration = fixture.service.read(&fixture.request, &CollectionCancellation::default()).await.unwrap();
    let used = calibration.metrics();
    assert!(used.queries > 1 && used.response_bytes >= 1024 && used.proof_bytes > 0);
    let largest_row = u64::from(fixture.largest_row_response_bytes);
    assert!(largest_row > 0);
    let original_history = fixture.objects.snapshot();
    for limit in ["queries", "responses", "proofs"] {
        let mut budget = fixture.request.budget;
        match limit {
            "queries" => budget.max_queries = used.queries * 2 - 1,
            "responses" => {
                // Every first-read prefix plus a required row fits. Charging the actual response leaves less
                // than the largest authenticated row bound, so the next read cannot receive a fresh allowance.
                budget.max_bytes = used.response_bytes.checked_add(largest_row).unwrap() - 1;
                budget.page_bytes = u64::from(budget.page_bytes).min(budget.max_bytes).try_into().unwrap();
            }
            "proofs" => budget.max_proof_bytes = used.proof_bytes * 2 - 1,
            _ => unreachable!(),
        }
        let cancellation = CollectionCancellation::default();
        let mut session = fixture.service.session(budget, &cancellation).unwrap();
        let before = fixture.store.requests.load(Ordering::SeqCst);
        let first = session.read(&fixture.request.range_id, &fixture.request.families).await.unwrap_or_else(|error| {
            panic!(
                "first {limit} read must fit: wire={}, planned_row={largest_row}, budget={budget:?}, error={error:?}",
                used.response_bytes
            )
        });
        assert_eq!(first.metrics().queries, used.queries);
        assert_eq!(first.metrics().response_bytes, used.response_bytes);
        assert_eq!(
            first.metrics().proof_bytes,
            used.proof_bytes,
            "cached proofs still consume the declared verification allowance"
        );
        if limit == "responses" {
            assert_eq!(budget.max_bytes - first.metrics().response_bytes, largest_row - 1);
        }
        let error = session.read(&fixture.request.range_id, &fixture.request.families).await.unwrap_err();
        assert_eq!(error.detail, Some(ErrorDetail::Delivery(DeliveryProblem::Capacity)), "{limit}");
        if limit == "queries" {
            assert_eq!(fixture.store.requests.load(Ordering::SeqCst) - before, budget.max_queries as usize);
        }
        assert_poisoned(&mut session, &fixture).await;
    }
    assert_eq!(fixture.objects.snapshot(), original_history);
}

#[tokio::test]
async fn whole_recovery_objects_and_bytes_are_not_refilled_for_the_next_capture_read() {
    let (source, records) = culprit_collector::analytical::test_support::object_definition_recovery_only_evidence();
    let mut fixture = Fixture::capture(source, records).await;
    fixture.request.families = vec![EvidenceFamily::ObjectDefinitions];
    fixture.request.budget.recovery_payloads = Some(RecoveryPayloadBudget::default());
    fixture.objects.allow_payloads.store(true, Ordering::SeqCst);
    let calibration = fixture.service.read(&fixture.request, &CollectionCancellation::default()).await.unwrap();
    assert!(calibration.is_read_complete());
    assert_eq!(calibration.metrics().recovery_objects, 1);
    let object_bytes = calibration.metrics().recovery_bytes;
    assert!(object_bytes > 1024);
    let original_history = fixture.objects.snapshot();
    for objects_exhausted in [true, false] {
        let mut budget = fixture.request.budget;
        budget.recovery_payloads = Some(RecoveryPayloadBudget {
            max_objects: if objects_exhausted { 1 } else { 3 },
            max_bytes: if objects_exhausted { object_bytes * 3 } else { object_bytes * 2 - 1 },
        });
        let cancellation = CollectionCancellation::default();
        let mut session = fixture.service.session(budget, &cancellation).unwrap();
        let before = payload_reads(&fixture);
        let first = session.read(&fixture.request.range_id, &fixture.request.families).await.unwrap();
        assert!(first.is_read_complete());
        assert_eq!(first.metrics().recovery_objects, 1);
        assert_eq!(first.metrics().recovery_bytes, object_bytes);
        let error = session.read(&fixture.request.range_id, &fixture.request.families).await.unwrap_err();
        assert_eq!(error.detail, Some(ErrorDetail::Delivery(DeliveryProblem::Capacity)));
        assert_eq!(payload_reads(&fixture) - before, 1, "the second object's full byte count is checked before open");
        assert_poisoned(&mut session, &fixture).await;
    }
    assert_eq!(fixture.objects.snapshot(), original_history);
}

#[tokio::test(start_paused = true)]
async fn idle_time_exhausts_the_original_deadline_before_any_new_recovery_or_warehouse_access() {
    let fixture = Fixture::new().await;
    let cancellation = CollectionCancellation::default();
    let mut session = fixture
        .service
        .session(EvidenceReadBudget { timeout_ms: 100, ..fixture.request.budget }, &cancellation)
        .unwrap();
    session.read(&fixture.request.range_id, &fixture.request.families).await.unwrap();
    tokio::time::advance(Duration::from_millis(100)).await;
    let before = io_counts(&fixture);
    let error = session.read(&fixture.request.range_id, &fixture.request.families).await.unwrap_err();
    assert_eq!(error.detail, Some(ErrorDetail::CollectionStopped(CollectionStop::TimedOut)));
    assert_eq!(io_counts(&fixture), before);
    assert_poisoned(&mut session, &fixture).await;
}

#[tokio::test(start_paused = true)]
async fn later_recovery_work_gets_only_the_unspent_absolute_time_and_drops_its_owner_on_timeout() {
    let fixture = Fixture::new().await;
    let cancellation = CollectionCancellation::default();
    let mut session = fixture
        .service
        .session(EvidenceReadBudget { timeout_ms: 100, ..fixture.request.budget }, &cancellation)
        .unwrap();
    session.read(&fixture.request.range_id, &fixture.request.families).await.unwrap();
    tokio::time::advance(Duration::from_millis(80)).await;
    fixture.objects.stall.store(true, Ordering::SeqCst);
    let started = Instant::now();
    let before = io_counts(&fixture);
    let error = session.read(&fixture.request.range_id, &fixture.request.families).await.unwrap_err();
    assert_eq!(error.detail, Some(ErrorDetail::CollectionStopped(CollectionStop::TimedOut)));
    assert_eq!(Instant::now() - started, Duration::from_millis(20), "read two cannot restart the caller's deadline");
    assert!(io_counts(&fixture).0 > before.0, "the test must start the owned recovery operation");
    assert_eq!(fixture.objects.active.load(Ordering::SeqCst), 0);
    fixture.objects.stall.store(false, Ordering::SeqCst);
    assert_poisoned(&mut session, &fixture).await;
}

#[tokio::test(start_paused = true)]
async fn cancellation_drops_a_later_owned_payload_body_and_permanently_closes_the_session() {
    let (source, records) = culprit_collector::analytical::test_support::object_definition_recovery_only_evidence();
    let mut fixture = Fixture::capture(source, records).await;
    fixture.request.families = vec![EvidenceFamily::ObjectDefinitions];
    fixture.request.budget.recovery_payloads = Some(RecoveryPayloadBudget::default());
    fixture.objects.allow_payloads.store(true, Ordering::SeqCst);
    let cancellation = CollectionCancellation::default();
    let mut session = fixture.service.session(fixture.request.budget, &cancellation).unwrap();
    session.read(&fixture.request.range_id, &fixture.request.families).await.unwrap();
    fixture.objects.stall_payload_body.store(true, Ordering::SeqCst);
    let original_history = fixture.objects.snapshot();
    let before = payload_reads(&fixture);
    let (result, _) = tokio::join!(session.read(&fixture.request.range_id, &fixture.request.families), async {
        fixture.objects.payload_started.notified().await;
        assert_eq!(fixture.objects.active.load(Ordering::SeqCst), 1);
        cancellation.cancel();
    });
    assert_eq!(result.unwrap_err().detail, Some(ErrorDetail::CollectionStopped(CollectionStop::Cancelled)));
    assert_eq!(payload_reads(&fixture) - before, 1);
    assert_eq!(fixture.objects.active.load(Ordering::SeqCst), 0);
    assert_poisoned(&mut session, &fixture).await;
    assert_eq!(fixture.objects.snapshot(), original_history);
}

#[tokio::test]
async fn a_cancelled_or_failed_action_cannot_retry_even_when_the_original_failure_is_removed() {
    for cancelled in [true, false] {
        let fixture = Fixture::new().await;
        let cancellation = CollectionCancellation::default();
        if cancelled {
            cancellation.cancel();
        } else {
            fixture.store.denied.store(true, Ordering::SeqCst);
        }
        let mut session = fixture.service.session(fixture.request.budget, &cancellation).unwrap();
        let before = io_counts(&fixture);
        let error = session.read(&fixture.request.range_id, &fixture.request.families).await.unwrap_err();
        if cancelled {
            assert_eq!(error.detail, Some(ErrorDetail::CollectionStopped(CollectionStop::Cancelled)));
            assert_eq!(io_counts(&fixture), before);
        } else {
            assert_eq!(error.code, culprit_core::ErrorCode::PermissionDenied);
            assert!(io_counts(&fixture).1 > before.1);
            fixture.store.denied.store(false, Ordering::SeqCst);
        }
        assert_poisoned(&mut session, &fixture).await;
    }
}
