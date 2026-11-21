use super::*;
use culprit_collector::analytical::test_support::{IndexedViewsCase, indexed_views_evidence};
#[tokio::test]
async fn original_view_catalog_is_authenticated_once_and_replayed_independently_of_current_scan_objects() {
    for case in IndexedViewsCase::ALL {
        let (source, rows) = indexed_views_evidence(case);
        let captured = Captured::from_source("shop", source.clone(), rows).await;
        let (_home, workspace, state) = workspace(snapshots(), SourceShutdown::Quiescent);
        let result = crate::observed::run(&workspace, culprit_core::ProviderReadKind::Scan, async |operation| {
            workspace
                .scan_populations_with_completed_capture(
                    "shop",
                    &options(),
                    CompletedCaptureScanInput {
                        analysis: CompletedCaptureAnalysis::IndexedViews,
                        ..captured.input(&captured.outbox.delivery(), &CollectionCancellation::default())
                    },
                    operation,
                )
                .await
        })
        .await
        .unwrap();
        let [RecordedOperationalRead::Captured { read }] =
            result.requested.snapshot.operations.indexed_view_originals.as_slice()
        else {
            panic!()
        };
        assert_eq!(read.description, source);
        assert!(read.is_valid());
        assert_eq!(result.requested.indexed_views.len(), 1);
        assert_eq!(result.requested.indexed_views[0].context.as_ref().unwrap(), &read.context().unwrap());
        assert!(result.requested.snapshot.operations.indexed_views.is_none());
        assert!(result.populations.iter().all(|report| report.snapshot.operations.indexed_view_originals.is_empty()));
        assert_eq!(state.active.load(Ordering::SeqCst), 0);
        assert_eq!(state.acquisitions.load(Ordering::SeqCst), 1);
        let expected = serde_json::to_value(&result.requested.indexed_views).unwrap();
        let id = result.requested.snapshot.id.clone();
        drop(captured);
        let replay = workspace.report_at("shop", &id).unwrap();
        assert_eq!(serde_json::to_value(&replay.indexed_views).unwrap(), expected);
        assert_eq!(serde_json::to_value(culprit_engine::reports::display(&replay)).unwrap()["indexed_views"], expected);
        assert_eq!(state.factories.load(Ordering::SeqCst), 1);
    }
}
#[tokio::test]
async fn sharing_withdraws_original_view_proof_without_rebinding_it_to_the_current_table_catalog() {
    let (source, rows) = indexed_views_evidence(IndexedViewsCase::Present);
    let captured = Captured::from_source("shop", source, rows).await;
    let (_home, workspace, _) = workspace(snapshots(), SourceShutdown::Quiescent);
    let report = crate::observed::run(&workspace, culprit_core::ProviderReadKind::Scan, async |operation| {
        workspace
            .scan_with_completed_capture(
                "shop",
                &options(),
                CompletedCaptureScanInput {
                    analysis: CompletedCaptureAnalysis::IndexedViews,
                    ..captured.input(&captured.outbox.delivery(), &CollectionCancellation::default())
                },
                operation,
            )
            .await
    })
    .await
    .unwrap();
    let snapshot = culprit_core::redact::redact_snapshot(&report.snapshot);
    assert!(matches!(
        snapshot.operations.indexed_view_originals.as_slice(),
        [RecordedOperationalRead::Unavailable { .. }]
    ));
    let replay = culprit_checks::analyze(serde_json::from_slice(&serde_json::to_vec(&snapshot).unwrap()).unwrap());
    let [withdrawn] = replay.indexed_views.as_slice() else { panic!() };
    assert!(withdrawn.entries.is_empty() && withdrawn.context.is_none());
    assert_eq!(withdrawn.native_index_rows, None);
    assert_eq!(withdrawn.source.state, SourceState::Unavailable);
    let raw = serde_json::to_string(&snapshot.operations.indexed_view_originals).unwrap();
    for private in ["manifest_sha256", "source_sha256", "parent_object", "observation_key", "epoch"] {
        assert!(!raw.contains(private), "{private}");
    }
}
#[tokio::test]
async fn cancelled_or_unconfirmed_scan_cannot_persist_a_view_original_or_native_measurement() {
    for cause in ["cancelled", "unconfirmed"] {
        let (source, rows) = indexed_views_evidence(IndexedViewsCase::Present);
        let captured = Captured::from_source("shop", source, rows).await;
        let (_home, workspace, state) = workspace(
            snapshots(),
            if cause == "unconfirmed" { SourceShutdown::Unconfirmed } else { SourceShutdown::Quiescent },
        );
        let cancellation = CollectionCancellation::default();
        if cause == "cancelled" {
            cancellation.cancel();
        }
        let result = crate::observed::run(&workspace, culprit_core::ProviderReadKind::Scan, async |operation| {
            workspace
                .scan_with_completed_capture(
                    "shop",
                    &options(),
                    CompletedCaptureScanInput {
                        analysis: CompletedCaptureAnalysis::IndexedViews,
                        ..captured.input(&captured.outbox.delivery(), &cancellation)
                    },
                    operation,
                )
                .await
        })
        .await;
        assert!(result.is_err(), "{cause}");
        assert_eq!(state.active.load(Ordering::SeqCst), 0);
        assert!(workspace.report_history("shop").unwrap().is_empty());
    }
}
