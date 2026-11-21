use super::*;
use culprit_collector::analytical::test_support::{ManagedInstanceResourceCase, managed_instance_resource_case};
#[tokio::test]
async fn selected_original_mi_history_settles_once_and_survives_source_free_saved_report_reanalysis() {
    for case in ManagedInstanceResourceCase::ALL {
        let (source, rows) = managed_instance_resource_case(case);
        let captured = Captured::from_source("shop", source.clone(), rows).await;
        let (_home, workspace, state) = workspace(snapshots(), SourceShutdown::Quiescent);
        let result = workspace
            .scan_populations_with_completed_capture(
                "shop",
                &options(),
                CompletedCaptureScanInput {
                    analysis: CompletedCaptureAnalysis::ManagedInstanceResources,
                    ..captured.input(&captured.outbox.delivery(), &CollectionCancellation::default())
                },
            )
            .await
            .unwrap();
        let Some(RecordedManagedInstanceResourceRead::Captured { read }) =
            result.requested.snapshot.operations.managed_instance_resources.as_deref()
        else {
            panic!()
        };
        assert!(read.is_valid());
        assert_eq!(read.description, source);
        assert_eq!(state.active.load(Ordering::SeqCst), 0);
        assert_eq!(state.acquisitions.load(Ordering::SeqCst), 1);
        assert!(
            result.populations.iter().all(|report| report.snapshot.operations.managed_instance_resources.is_none()
                && report.managed_instance_resources.is_none())
        );
        let expected = serde_json::to_value(&result.requested.managed_instance_resources).unwrap();
        let id = result.requested.snapshot.id.clone();
        drop(captured);
        let replay = workspace.report_at("shop", &id).unwrap();
        assert_eq!(serde_json::to_value(&replay.managed_instance_resources).unwrap(), expected);
        assert_eq!(
            serde_json::to_value(culprit_engine::reports::display(&replay)).unwrap()["managed_instance_resources"],
            expected
        );
        assert_eq!(state.factories.load(Ordering::SeqCst), 1);
        let shared = culprit_core::redact::redact_snapshot(&replay.snapshot);
        let shared = culprit_checks::analyze(shared);
        let report = shared.managed_instance_resources.unwrap();
        assert_eq!(report.source_state, SourceState::Unavailable);
        assert!(report.intervals.is_empty());
        assert!(report.native_rows.is_none() && report.observed_at.is_none());
    }
}
#[tokio::test]
async fn a_foreign_original_receipt_or_connection_is_refused_before_any_scan_factory_opens() {
    let (source, rows) = managed_instance_resource_case(ManagedInstanceResourceCase::Complete);
    let captured = Captured::from_source("foreign-saved-connection", source, rows).await;
    let (_home, workspace, state) = workspace(snapshots(), SourceShutdown::Quiescent);
    let result = workspace
        .scan_with_completed_capture(
            "shop",
            &options(),
            CompletedCaptureScanInput {
                analysis: CompletedCaptureAnalysis::ManagedInstanceResources,
                ..captured.input(&captured.outbox.delivery(), &CollectionCancellation::default())
            },
        )
        .await;
    assert!(result.is_err());
    assert_eq!(state.factories.load(Ordering::SeqCst), 0);
}
