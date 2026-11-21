use super::*;
use crate::analytical::test_support::{LocalCaptureFixture, page_server_read_evidence_cases};
use culprit_core::{ErrorDetail, limits::OperationGuard};
use std::time::Duration;

async fn fixture(case: &str) -> LocalCaptureFixture {
    let case = page_server_read_evidence_cases().into_iter().find(|value| value.name == case).unwrap();
    LocalCaptureFixture::query_history(case.source, case.records).await
}
async fn read(fixture: &LocalCaptureFixture, budget: CompletedCaptureBudget) -> Result<CompletedQueryHistory> {
    let guard = OperationGuard::new(budget.timeout).unwrap();
    fixture
        .outbox
        .as_ref()
        .unwrap()
        .delivery()
        .with_completed_query_history(
            CompletedCaptureLookup {
                connection: "local-evidence",
                observation_id: &fixture.manifest.capture.observation_id,
            },
            QueryHistoryReadKind::Runtime,
            budget,
            guard.handle(),
            (),
            Ok,
        )
        .await
}

#[tokio::test]
async fn ready_history_keeps_original_receipts_counts_and_unresolved_positions_in_one_verified_pass() {
    for case in ["native_fragment", "empty", "single_fragment_with_definitions"] {
        let fixture = fixture(case).await;
        assert_eq!(fixture.manifest.status, CollectionStatus::Complete);
        let result = read(&fixture, CompletedCaptureBudget::default()).await.unwrap();
        assert_eq!(result.source(), fixture.manifest.description.as_ref().unwrap());
        assert_eq!(result.capture().capture, fixture.manifest.capture);
        assert_eq!(result.reference_coverage(), &fixture.manifest.references);
        assert_eq!(result.bytes_read(), fixture.original_bytes());
        assert_eq!(result.collected_from(), fixture.manifest.started_at);
        assert_eq!(Some(result.collected_until()), fixture.manifest.finished_at.as_deref());
        for stream in [TelemetryStream::QueryRuntime, TelemetryStream::QueryDefinitions] {
            let original = fixture.manifest.files.iter().find(|file| file.stream == stream).unwrap();
            assert_eq!(result.captured_rows(stream), Some(original.rows));
            let selected =
                result.records().iter().zip(result.row_ordinals()).filter(|(row, _)| row.record.stream() == stream);
            for (index, (row, ordinal)) in selected.enumerate() {
                assert_eq!(*ordinal as usize, index);
                assert_eq!(row.capture, fixture.manifest.capture);
            }
        }
        if case == "native_fragment" {
            assert!(result.records()[0].record_id.is_none());
            assert!(!result.reference_coverage().complete);
            assert!(result.reference_coverage().integrity_complete);
        }
    }
}

#[tokio::test]
async fn row_and_all_artifact_byte_bounds_refuse_without_truncation_or_quarantine() {
    let fixture = fixture("single_fragment_with_definitions").await;
    let exact = CompletedCaptureBudget { max_rows: 2, max_bytes: fixture.original_bytes(), ..Default::default() };
    assert_eq!(read(&fixture, exact).await.unwrap().records().len(), 2);
    for budget in [
        CompletedCaptureBudget { max_rows: 1, ..exact },
        CompletedCaptureBudget { max_bytes: exact.max_bytes - 1, ..exact },
    ] {
        assert_eq!(
            read(&fixture, budget).await.unwrap_err().detail,
            Some(ErrorDetail::Delivery(DeliveryProblem::Capacity))
        );
    }
    assert_eq!(
        fixture.outbox.as_ref().unwrap().status(1, None).await.unwrap().entries[0].state,
        OutboxCaptureState::Ready
    );
}

#[tokio::test]
async fn journal_connection_and_settled_owner_cannot_be_substituted_by_tool_arguments() {
    let fixture = fixture("native_fragment").await;
    let guard = OperationGuard::new(Duration::from_secs(1)).unwrap();
    let error = fixture
        .outbox
        .as_ref()
        .unwrap()
        .delivery()
        .with_completed_query_history(
            CompletedCaptureLookup {
                connection: "foreign-connection",
                observation_id: &fixture.manifest.capture.observation_id,
            },
            QueryHistoryReadKind::Runtime,
            CompletedCaptureBudget::default(),
            guard.handle(),
            (),
            Ok,
        )
        .await
        .unwrap_err();
    assert_eq!(error.detail, Some(ErrorDetail::Outbox(OutboxProblem::MissingCapture)));
    let case = page_server_read_evidence_cases().remove(0);
    let unsettled = LocalCaptureFixture::with_shutdown(case.source, case.records, SourceShutdown::Unconfirmed).await;
    assert_eq!(
        read(&unsettled, CompletedCaptureBudget::default()).await.unwrap_err().detail,
        Some(ErrorDetail::Outbox(OutboxProblem::SourceUnconfirmed))
    );
}

#[tokio::test]
async fn cancelled_lookup_and_post_verification_projection_never_publish_success() {
    let fixture = fixture("native_fragment").await;
    let guard = OperationGuard::new(Duration::from_secs(1)).unwrap();
    let operation = guard.handle();
    drop(guard);
    let error = fixture
        .outbox
        .as_ref()
        .unwrap()
        .delivery()
        .with_completed_query_history::<()>(
            CompletedCaptureLookup {
                connection: "local-evidence",
                observation_id: &fixture.manifest.capture.observation_id,
            },
            QueryHistoryReadKind::Runtime,
            CompletedCaptureBudget::default(),
            operation,
            (),
            |_| panic!("cancelled lookup must not project"),
        )
        .await
        .unwrap_err();
    assert_eq!(error.detail, Some(ErrorDetail::CollectionStopped(CollectionStop::Cancelled)));
    let guard = OperationGuard::new(Duration::from_secs(1)).unwrap();
    let operation = guard.handle();
    let error = fixture
        .outbox
        .as_ref()
        .unwrap()
        .delivery()
        .with_completed_query_history(
            CompletedCaptureLookup {
                connection: "local-evidence",
                observation_id: &fixture.manifest.capture.observation_id,
            },
            QueryHistoryReadKind::Runtime,
            CompletedCaptureBudget::default(),
            operation,
            (),
            move |original| {
                assert_eq!(original.records().len(), 1);
                drop(guard);
                Ok(())
            },
        )
        .await
        .unwrap_err();
    assert_eq!(error.detail, Some(ErrorDetail::CollectionStopped(CollectionStop::Cancelled)));
    assert!(read(&fixture, CompletedCaptureBudget::default()).await.is_ok());
}

#[tokio::test]
async fn original_deadline_and_cancellation_include_waiting_for_the_existing_outbox_actor() {
    let fixture = fixture("native_fragment").await;
    tokio::time::pause();
    let outbox = fixture.outbox.as_ref().unwrap();
    let locked = outbox.files.acquire().await;
    for cancelled in [false, true] {
        let guard = OperationGuard::new(Duration::from_millis(1)).unwrap();
        let operation = guard.handle();
        let delivery = outbox.delivery();
        let id = fixture.manifest.capture.observation_id.clone();
        let running = tokio::spawn(async move {
            delivery
                .with_completed_query_history::<()>(
                    CompletedCaptureLookup { connection: "local-evidence", observation_id: &id },
                    QueryHistoryReadKind::Runtime,
                    CompletedCaptureBudget::default(),
                    operation,
                    (),
                    |_| panic!("a stopped queued action cannot reach projection"),
                )
                .await
        });
        tokio::task::yield_now().await;
        if cancelled {
            drop(guard);
        }
        tokio::time::advance(Duration::from_millis(2)).await;
        let error = running.await.unwrap().unwrap_err();
        assert_eq!(
            error.detail,
            Some(ErrorDetail::CollectionStopped(if cancelled {
                CollectionStop::Cancelled
            } else {
                CollectionStop::TimedOut
            }))
        );
    }
    drop(locked);
    assert_eq!(outbox.status(1, None).await.unwrap().entries[0].state, OutboxCaptureState::Ready);
}

#[tokio::test]
async fn a_started_canonical_worker_is_joined_before_a_cancelled_projection_can_release_ownership() {
    let fixture = fixture("native_fragment").await;
    let guard = OperationGuard::new(Duration::from_secs(5)).unwrap();
    let operation = guard.handle();
    let delivery = fixture.outbox.as_ref().unwrap().delivery();
    let id = fixture.manifest.capture.observation_id.clone();
    let (started, arrived) = tokio::sync::oneshot::channel();
    let (release, released) = std::sync::mpsc::sync_channel(1);
    let running = tokio::spawn(async move {
        delivery
            .with_completed_query_history(
                CompletedCaptureLookup { connection: "local-evidence", observation_id: &id },
                QueryHistoryReadKind::Runtime,
                CompletedCaptureBudget::default(),
                operation,
                (),
                move |original| {
                    assert_eq!(original.records().len(), 1);
                    started.send(()).unwrap();
                    released.recv_timeout(Duration::from_secs(2)).unwrap();
                    Ok(())
                },
            )
            .await
    });
    arrived.await.unwrap();
    drop(guard);
    tokio::task::yield_now().await;
    assert!(!running.is_finished());
    release.send(()).unwrap();
    let error = running.await.unwrap().unwrap_err();
    assert_eq!(error.detail, Some(ErrorDetail::CollectionStopped(CollectionStop::Cancelled)));
    assert!(read(&fixture, CompletedCaptureBudget::default()).await.is_ok());
}

#[tokio::test]
async fn corruption_of_an_unselected_plan_body_is_still_integrity_failure_and_quarantines_the_original() {
    let fixture = fixture("single_fragment_with_definitions").await;
    let plan = fixture.manifest.files.iter().find(|file| file.stream == TelemetryStream::PlanDefinitions).unwrap();
    let path = fixture.capture_directory.join(&plan.file);
    let mut bytes = std::fs::read(&path).unwrap();
    bytes[0] ^= 1;
    std::fs::write(path, bytes).unwrap();
    assert!(read(&fixture, CompletedCaptureBudget::default()).await.is_err());
    assert_eq!(
        fixture.outbox.as_ref().unwrap().status(1, None).await.unwrap().entries[0].state,
        OutboxCaptureState::Quarantined
    );
}
