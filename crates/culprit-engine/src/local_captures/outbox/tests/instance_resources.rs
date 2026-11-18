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
