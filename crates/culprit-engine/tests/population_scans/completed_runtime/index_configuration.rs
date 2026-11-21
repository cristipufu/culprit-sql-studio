use super::*;
use culprit_collector::analytical::test_support::{IndexConfigurationCase, index_configuration_evidence};

#[tokio::test]
async fn original_catalog_configuration_survives_scan_shutdown_and_saved_reanalysis_without_recovery() {
    for case in IndexConfigurationCase::ALL {
        let (source, rows) = index_configuration_evidence(case);
        let dataset = source.observation.snapshot().unwrap().dataset;
        let captured = Captured::from_source("shop", source.clone(), rows).await;
        let (_home, workspace, state) = workspace(snapshots(), SourceShutdown::Quiescent);
        let result = workspace
            .scan_populations_with_completed_capture(
                "shop",
                &options(),
                CompletedCaptureScanInput {
                    analysis: CompletedCaptureAnalysis::IndexConfiguration { dataset },
                    ..captured.input(&captured.outbox.delivery(), &CollectionCancellation::default())
                },
            )
            .await
            .unwrap();
        let [RecordedOperationalRead::Captured { read }] =
            result.requested.snapshot.operations.index_configuration.as_slice()
        else {
            panic!()
        };
        assert_eq!(read.description, source);
        assert!(read.is_valid());
        let expected_context = read.context().unwrap();
        let originals =
            result.requested.index_configuration.iter().filter(|report| report.context.is_some()).collect::<Vec<_>>();
        let [original] = originals.as_slice() else { panic!() };
        assert_eq!(original.context.as_ref().unwrap(), &expected_context);
        assert_eq!(state.active.load(Ordering::SeqCst), 0);
        assert_eq!(state.acquisitions.load(Ordering::SeqCst), 1);
        assert!(result.populations.iter().all(|report| report.snapshot.operations.index_configuration.is_empty()));
        let expected = serde_json::to_value(&result.requested.index_configuration).unwrap();
        let id = result.requested.snapshot.id.clone();
        drop(captured);
        let replay = workspace.report_at("shop", &id).unwrap();
        assert_eq!(serde_json::to_value(&replay.index_configuration).unwrap(), expected);
        assert_eq!(
            serde_json::to_value(culprit_engine::reports::display(&replay)).unwrap()["index_configuration"],
            expected
        );
        assert_eq!(state.factories.load(Ordering::SeqCst), 1);
    }
}
#[tokio::test]
async fn sharing_withdraws_each_original_catalog_proof_while_scan_configuration_remains_independent() {
    let (source, rows) = index_configuration_evidence(IndexConfigurationCase::Mixed);
    let captured = Captured::from_source("shop", source, rows).await;
    let (_home, workspace, _) = workspace(snapshots(), SourceShutdown::Quiescent);
    let report = workspace
        .scan_with_completed_capture(
            "shop",
            &options(),
            CompletedCaptureScanInput {
                analysis: CompletedCaptureAnalysis::IndexConfiguration { dataset: SnapshotDataset::Indexes },
                ..captured.input(&captured.outbox.delivery(), &CollectionCancellation::default())
            },
        )
        .await
        .unwrap();
    let snapshot = culprit_core::redact::redact_snapshot(&report.snapshot);
    assert!(matches!(
        snapshot.operations.index_configuration.as_slice(),
        [RecordedOperationalRead::Unavailable { .. }]
    ));
    let replay = culprit_checks::analyze(serde_json::from_slice(&serde_json::to_vec(&snapshot).unwrap()).unwrap());
    let withdrawn = replay.index_configuration.iter().find(|report| report.dataset.is_none()).unwrap();
    assert!(withdrawn.entries.is_empty() && withdrawn.context.is_none());
    assert_eq!(withdrawn.native_rows, None);
    assert_eq!(withdrawn.source_state, Some(SourceState::Unavailable));
    let raw = serde_json::to_string(&snapshot.operations.index_configuration).unwrap();
    for key in ["manifest_sha256", "source_sha256", "observation_key", "9007199254740994", "epoch"] {
        assert!(!raw.contains(key), "{key}");
    }
}
#[tokio::test]
async fn unconfirmed_scan_and_cancelled_original_never_attach_catalog_configuration() {
    for cause in ["cancelled", "unconfirmed"] {
        let (source, rows) = index_configuration_evidence(IndexConfigurationCase::Heap);
        let captured = Captured::from_source("shop", source, rows).await;
        let (_home, workspace, state) = workspace(
            snapshots(),
            if cause == "unconfirmed" { SourceShutdown::Unconfirmed } else { SourceShutdown::Quiescent },
        );
        let cancellation = CollectionCancellation::default();
        if cause == "cancelled" {
            cancellation.cancel();
        }
        let result = workspace
            .scan_with_completed_capture(
                "shop",
                &options(),
                CompletedCaptureScanInput {
                    analysis: CompletedCaptureAnalysis::IndexConfiguration { dataset: SnapshotDataset::Tables },
                    ..captured.input(&captured.outbox.delivery(), &cancellation)
                },
            )
            .await;
        assert!(result.is_err(), "{cause}");
        assert_eq!(state.active.load(Ordering::SeqCst), 0);
        assert!(workspace.report_history("shop").unwrap().is_empty());
    }
}
