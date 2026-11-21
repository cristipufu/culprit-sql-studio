use super::*;
use culprit_collector::analytical::test_support::{InstanceResourceCase, instance_resource_evidence};

#[tokio::test]
async fn local_shared_instance_points_keep_the_same_original_owner_all_rows_and_independent_native_gaps() {
    for case in InstanceResourceCase::ALL {
        let (source, rows) = instance_resource_evidence(case);
        let fixture = LocalCaptureFixture::snapshot(source.clone(), rows).await;
        let reader = OutboxCaptureReader::delivery(
            fixture.outbox.as_ref().unwrap().delivery(),
            CompletedCaptureBudget::default(),
        )
        .unwrap();
        let guard = OperationGuard::new(reader.timeout()).unwrap();
        let value = reader
            .analyze(LocalCaptureRequest {
                connection: "local-evidence",
                observation_id: &fixture.manifest.capture.observation_id,
                analysis: LocalCaptureAnalysisKind::InstanceResources,
                operation: guard.handle(),
            })
            .await
            .unwrap();
        assert_eq!(value.kind(), LocalCaptureAnalysisKind::InstanceResources);
        let LocalCapturedAnalysis::Snapshot(value) = value else { panic!() };
        assert_eq!(value.original().read().description, source);
        assert_eq!(value.original().connection(), "local-evidence");
        assert_eq!(value.original().read().origin.capture.capture, fixture.manifest.capture);
        assert_eq!(value.original().bytes_read(), fixture.original_bytes());
        let LocalSnapshotReport::InstanceResources(report) = value.report() else { panic!() };
        assert_eq!(
            report,
            &Box::new(culprit_checks::instance_resources::explain_captured(value.original().read()).unwrap())
        );
        assert_eq!(report.read_complete, Some(true));
        assert!(report.context.as_ref().unwrap().native().unwrap().target.database().is_none());
    }
}

#[tokio::test]
async fn cold_instance_point_refuses_foreign_journal_cancel_byte_budget_and_modified_original() {
    let (source, rows) = instance_resource_evidence(InstanceResourceCase::Exact);
    let mut fixture = LocalCaptureFixture::snapshot(source, rows).await;
    drop(fixture.outbox.take());
    let reader = OutboxCaptureReader::directory(fixture.directory.clone(), CompletedCaptureBudget::default()).unwrap();
    for cancelled in [false, true] {
        let guard = OperationGuard::new(reader.timeout()).unwrap();
        let operation = guard.handle();
        if cancelled {
            drop(guard);
        }
        let error = reader
            .analyze(LocalCaptureRequest {
                connection: if cancelled { "local-evidence" } else { "foreign" },
                observation_id: &fixture.manifest.capture.observation_id,
                analysis: LocalCaptureAnalysisKind::InstanceResources,
                operation,
            })
            .await
            .unwrap_err();
        assert_eq!(
            error.detail,
            Some(if cancelled {
                ErrorDetail::CollectionStopped(CollectionStop::Cancelled)
            } else {
                ErrorDetail::Outbox(OutboxProblem::MissingCapture)
            })
        );
    }
    let tiny = OutboxCaptureReader::directory(
        fixture.directory.clone(),
        CompletedCaptureBudget { max_bytes: 1, ..Default::default() },
    )
    .unwrap();
    let guard = OperationGuard::new(tiny.timeout()).unwrap();
    let error = tiny
        .analyze(LocalCaptureRequest {
            connection: "local-evidence",
            observation_id: &fixture.manifest.capture.observation_id,
            analysis: LocalCaptureAnalysisKind::InstanceResources,
            operation: guard.handle(),
        })
        .await
        .unwrap_err();
    assert_eq!(error.detail, Some(ErrorDetail::Delivery(DeliveryProblem::Capacity)));
    let body = fixture.capture_directory.join(&fixture.manifest.files[0].file);
    let original = std::fs::read_to_string(&body).unwrap();
    let changed = original.replace("9007199254740993", "9007199254740992");
    assert_ne!(original, changed);
    std::fs::write(body, changed).unwrap();
    let guard = OperationGuard::new(reader.timeout()).unwrap();
    assert!(
        reader
            .analyze(LocalCaptureRequest {
                connection: "local-evidence",
                observation_id: &fixture.manifest.capture.observation_id,
                analysis: LocalCaptureAnalysisKind::InstanceResources,
                operation: guard.handle()
            })
            .await
            .is_err()
    );
}

#[tokio::test]
async fn cold_scheduler_originals_keep_the_native_row_counts_and_typed_detail_on_the_existing_capture_owner() {
    use culprit_collector::analytical::test_support::{SchedulerCase, instance_resource_scheduler_evidence};
    for case in SchedulerCase::ALL {
        let (source, rows) = instance_resource_scheduler_evidence(case);
        let mut fixture = LocalCaptureFixture::snapshot(source.clone(), rows).await;
        drop(fixture.outbox.take());
        let reader =
            OutboxCaptureReader::directory(fixture.directory.clone(), CompletedCaptureBudget::default()).unwrap();
        let guard = OperationGuard::new(reader.timeout()).unwrap();
        let analysis = reader
            .analyze(LocalCaptureRequest {
                connection: "local-evidence",
                observation_id: &fixture.manifest.capture.observation_id,
                analysis: LocalCaptureAnalysisKind::InstanceResources,
                operation: guard.handle(),
            })
            .await
            .unwrap();
        let LocalCapturedAnalysis::Snapshot(analysis) = analysis else { panic!() };
        assert_eq!(analysis.original().read().description, source);
        assert_eq!(analysis.original().read().origin.capture.capture, fixture.manifest.capture);
        assert_eq!(analysis.original().bytes_read(), fixture.original_bytes());
        let LocalSnapshotReport::InstanceResources(report) = analysis.report() else { panic!() };
        assert_eq!(
            report,
            &Box::new(culprit_checks::instance_resources::explain_captured(analysis.original().read()).unwrap())
        );
        let schedulers = report.schedulers.as_deref().unwrap();
        assert_eq!(schedulers.read_complete, Some(true));
        assert_eq!(schedulers.native_source.as_ref().unwrap().source, culprit_core::source_id::SCHEDULERS);
        assert_eq!(
            schedulers.native_rows,
            Some(if case == SchedulerCase::Denied {
                0
            } else if case == SchedulerCase::Limited {
                257
            } else {
                2
            })
        );
        assert!(schedulers.context.as_ref().unwrap().native().unwrap().target.database().is_none());
    }
}

#[tokio::test]
async fn cold_scheduler_analysis_refuses_changed_original_state_without_any_live_fallback() {
    use culprit_collector::analytical::test_support::{SchedulerCase, instance_resource_scheduler_evidence};
    let (source, rows) = instance_resource_scheduler_evidence(SchedulerCase::Exact);
    let mut fixture = LocalCaptureFixture::snapshot(source, rows).await;
    drop(fixture.outbox.take());
    let reader = OutboxCaptureReader::directory(fixture.directory.clone(), CompletedCaptureBudget::default()).unwrap();
    let body = fixture.capture_directory.join(&fixture.manifest.files[0].file);
    let original = std::fs::read_to_string(&body).unwrap();
    let changed = original.replace("HIDDEN ONLINE", "HIDDEN OFFLINE");
    assert_ne!(original, changed);
    std::fs::write(body, changed).unwrap();
    let guard = OperationGuard::new(reader.timeout()).unwrap();
    assert!(
        reader
            .analyze(LocalCaptureRequest {
                connection: "local-evidence",
                observation_id: &fixture.manifest.capture.observation_id,
                analysis: LocalCaptureAnalysisKind::InstanceResources,
                operation: guard.handle()
            })
            .await
            .is_err()
    );
}

#[tokio::test]
async fn cold_cpu_profile_uses_the_existing_local_instance_resource_interpreter_without_new_source_work() {
    for (source, rows) in culprit_collector::analytical::test_support::process_cpu_interval_evidence(
        culprit_collector::analytical::test_support::ProcessCpuIntervalCase::Exact,
    ) {
        let fixture = LocalCaptureFixture::snapshot(source.clone(), rows).await;
        let reader = OutboxCaptureReader::delivery(
            fixture.outbox.as_ref().unwrap().delivery(),
            CompletedCaptureBudget::default(),
        )
        .unwrap();
        let guard = OperationGuard::new(reader.timeout()).unwrap();
        let value = reader
            .analyze(LocalCaptureRequest {
                connection: "local-evidence",
                observation_id: &fixture.manifest.capture.observation_id,
                analysis: LocalCaptureAnalysisKind::InstanceResources,
                operation: guard.handle(),
            })
            .await
            .unwrap();
        let LocalCapturedAnalysis::Snapshot(value) = value else { panic!() };
        let LocalSnapshotReport::InstanceResources(report) = value.report() else { panic!() };
        assert_eq!(value.original().read().description, source);
        assert_eq!(report.process_cpu.as_ref().unwrap().populations.len(), 2);
        assert_eq!(report.process_cpu.as_ref().unwrap().capture.as_ref().unwrap().capture, fixture.manifest.capture);
    }
}
