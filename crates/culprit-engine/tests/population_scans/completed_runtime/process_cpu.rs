use super::*;
use culprit_collector::analytical::test_support::{ProcessCpuIntervalCase, process_cpu_interval_evidence};

#[tokio::test]
async fn two_explicit_cpu_originals_share_one_scan_owner_and_survive_offline_report_recovery() {
    let [(a, ar), (b, br)] = process_cpu_interval_evidence(ProcessCpuIntervalCase::Exact);
    let first = Captured::from_source("shop", a.clone(), ar).await;
    let second = Captured::from_source("shop", b.clone(), br).await;
    let (_home, workspace, state) = workspace(snapshots(), SourceShutdown::Quiescent);
    let cancellation = CollectionCancellation::default();
    let left = first.outbox.delivery();
    let right = second.outbox.delivery();
    let mut x = first.input(&left, &cancellation);
    x.analysis = CompletedCaptureAnalysis::ProcessCpu;
    let mut y = second.input(&right, &cancellation);
    y.analysis = CompletedCaptureAnalysis::ProcessCpu;
    let result = workspace
        .scan_populations_with_completed_captures(
            "shop",
            &options(),
            vec![x, y],
            CompletedCaptureBudget { max_rows: 14, ..Default::default() },
        )
        .await
        .unwrap();
    assert_eq!(state.factories.load(Ordering::SeqCst), 1);
    assert_eq!(state.acquisitions.load(Ordering::SeqCst), 1);
    assert_eq!(state.active.load(Ordering::SeqCst), 0);
    assert_eq!(result.requested.snapshot.operations.process_cpu_originals.len(), 2);
    let report = result.requested.instance_resources.as_ref().unwrap();
    assert!(report.process_cpu_interval.as_ref().unwrap().available);
    assert!(result.populations.iter().all(|report| report.snapshot.operations.process_cpu_originals.is_empty()));
    let expected = serde_json::to_value(report).unwrap();
    let id = result.requested.snapshot.id.clone();
    drop(first);
    drop(second);
    assert_eq!(serde_json::to_value(&workspace.report_at("shop", &id).unwrap().instance_resources).unwrap(), expected);
}
#[tokio::test]
async fn plural_row_or_byte_exhaustion_and_any_pre_cancelled_original_refuse_before_native_acquisition() {
    let [(a, ar), (b, br)] = process_cpu_interval_evidence(ProcessCpuIntervalCase::Exact);
    let first = Captured::from_source("shop", a, ar).await;
    let second = Captured::from_source("shop", b, br).await;
    for (rows, bytes, cancelled) in [(13, 8 * 1024 * 1024, false), (14, 1, false), (14, 8 * 1024 * 1024, true)] {
        let (_home, workspace, state) = workspace(snapshots(), SourceShutdown::Quiescent);
        let c = CollectionCancellation::default();
        let second_c = CollectionCancellation::default();
        if cancelled {
            second_c.cancel();
        }
        let left = first.outbox.delivery();
        let right = second.outbox.delivery();
        let mut x = first.input(&left, &c);
        x.analysis = CompletedCaptureAnalysis::ProcessCpu;
        let mut y = second.input(&right, &second_c);
        y.analysis = CompletedCaptureAnalysis::ProcessCpu;
        assert!(
            workspace
                .scan_populations_with_completed_captures(
                    "shop",
                    &options(),
                    vec![x, y],
                    CompletedCaptureBudget { max_rows: rows, max_bytes: bytes, ..Default::default() }
                )
                .await
                .is_err()
        );
        assert_eq!(state.factories.load(Ordering::SeqCst), 0);
        assert_eq!(state.acquisitions.load(Ordering::SeqCst), 0);
    }
}
#[tokio::test]
async fn conflicting_completed_cpu_attempts_are_rejected_before_any_factory_or_original_read() {
    let [(a, ar), _] = process_cpu_interval_evidence(ProcessCpuIntervalCase::Exact);
    let original = Captured::from_source("shop", a, ar).await;
    let delivery = original.outbox.delivery();
    let cancellation = CollectionCancellation::default();
    let mut conflict = original.attempt.clone();
    conflict.manifest_sha256 = Some("b".repeat(64));
    let mut first = original.input(&delivery, &cancellation);
    first.analysis = CompletedCaptureAnalysis::ProcessCpu;
    let second = CompletedCaptureScanInput {
        analysis: CompletedCaptureAnalysis::ProcessCpu,
        attempt: &conflict,
        ..original.input(&delivery, &cancellation)
    };
    let (_home, workspace, state) = workspace(snapshots(), SourceShutdown::Quiescent);
    let error = workspace
        .scan_with_completed_captures("shop", &options(), vec![first, second], CompletedCaptureBudget::default())
        .await
        .unwrap_err();
    assert_eq!(error.detail, Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::Conflict)));
    assert_eq!(state.factories.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn exact_repeated_originals_remain_idempotent_without_an_invented_two_point_interval() {
    let [(a, rows), _] = process_cpu_interval_evidence(ProcessCpuIntervalCase::Exact);
    let original = Captured::from_source("shop", a, rows).await;
    let delivery = original.outbox.delivery();
    let cancellation = CollectionCancellation::default();
    let mut first = original.input(&delivery, &cancellation);
    first.analysis = CompletedCaptureAnalysis::ProcessCpu;
    let mut second = original.input(&delivery, &cancellation);
    second.analysis = CompletedCaptureAnalysis::ProcessCpu;
    let (_home, workspace, state) = workspace(snapshots(), SourceShutdown::Quiescent);
    let result = workspace
        .scan_with_completed_captures(
            "shop",
            &options(),
            vec![first, second],
            CompletedCaptureBudget { max_rows: 14, ..Default::default() },
        )
        .await
        .unwrap();
    assert_eq!(result.snapshot.operations.process_cpu_originals.len(), 1);
    assert!(!result.instance_resources.as_ref().unwrap().process_cpu_interval.as_ref().unwrap().available);
    assert_eq!(state.factories.load(Ordering::SeqCst), 1);
    assert_eq!(state.acquisitions.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn expired_original_worker_queue_does_not_open_the_native_scan_factory() {
    use culprit_collector::outbox::CompletedCaptureLookup;
    use culprit_core::limits::OperationGuard;
    use std::time::Duration;
    let [(source, rows), _] = process_cpu_interval_evidence(ProcessCpuIntervalCase::Exact);
    let original = Captured::from_source("shop", source, rows).await;
    let delivery = original.outbox.delivery();
    let blocking_delivery = delivery.clone();
    let observation = original.attempt.capture.observation_id.clone();
    let entered = Arc::new(tokio::sync::Notify::new());
    let worker_entered = entered.clone();
    let (release, wait) = std::sync::mpsc::channel();
    let holding = tokio::spawn(async move {
        let guard = OperationGuard::new(Duration::from_secs(5)).unwrap();
        let operation = guard.handle();
        blocking_delivery
            .with_completed_snapshot(
                CompletedCaptureLookup { connection: "shop", observation_id: &observation },
                SnapshotDataset::ProcessCpu,
                CompletedCaptureBudget::default(),
                operation,
                guard,
                move |_| {
                    worker_entered.notify_one();
                    wait.recv_timeout(Duration::from_secs(1)).map_err(|_| CollectionStop::TimedOut.error())
                },
            )
            .await
    });
    entered.notified().await;
    let cancellation = CollectionCancellation::default();
    let mut input = original.input(&delivery, &cancellation);
    input.analysis = CompletedCaptureAnalysis::ProcessCpu;
    let (_home, workspace, state) = workspace(snapshots(), SourceShutdown::Quiescent);
    let outcome = workspace
        .scan_with_completed_captures(
            "shop",
            &options(),
            vec![input],
            CompletedCaptureBudget { timeout: Duration::from_millis(50), ..Default::default() },
        )
        .await;
    // Release and await the SAME local worker before inspecting failure, even when the facade failed.
    let release_result = release.send(());
    let settled = holding.await.unwrap();
    assert!(release_result.is_ok());
    settled.unwrap();
    assert_eq!(
        outcome.unwrap_err().detail,
        Some(culprit_core::ErrorDetail::CollectionStopped(CollectionStop::TimedOut))
    );
    assert_eq!(state.factories.load(Ordering::SeqCst), 0);
    assert_eq!(state.acquisitions.load(Ordering::SeqCst), 0);
}
