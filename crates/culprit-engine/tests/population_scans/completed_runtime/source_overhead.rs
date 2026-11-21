use super::*;
use culprit_collector::analytical::test_support::{LocalCaptureFixture, SourceAcquisitionCase};
use culprit_core::{HistoricalSourceOverhead, RecordedSourceAcquisition, limits::OperationGuard};
use culprit_engine::{
    LocalCaptureAnalysisKind, LocalCaptureReader, LocalCaptureRequest, LocalCapturedAnalysis, OutboxCaptureReader,
};
async fn read(connection: &str, fixture: &LocalCaptureFixture) -> Box<culprit_engine::LocalSourceOverhead> {
    let reader =
        OutboxCaptureReader::delivery(fixture.outbox.as_ref().unwrap().delivery(), Default::default()).unwrap();
    let guard = OperationGuard::new(reader.timeout()).unwrap();
    // This fixture deliberately binds its own journal label, independently from the current scan database.
    let result = reader
        .analyze(LocalCaptureRequest {
            connection,
            observation_id: &fixture.manifest.capture.observation_id,
            analysis: LocalCaptureAnalysisKind::SourceOverhead,
            operation: guard.handle(),
        })
        .await
        .unwrap();
    let LocalCapturedAnalysis::SourceOverhead(result) = result else { panic!() };
    result
}
#[tokio::test]
async fn foreign_journal_connection_cannot_attach_to_a_scan_even_when_the_current_database_has_matching_labels() {
    let fixture = LocalCaptureFixture::source_acquisition(SourceAcquisitionCase::Failed).await;
    let (home, workspace, state) = workspace(snapshots(), SourceShutdown::Quiescent);
    let captured = read("local-evidence", &fixture).await;
    // Existing workspace fixture's saved label is shop, while the local journal's label must remain exact.
    assert!(workspace.scan_with_captured_source_overhead("shop", &options(), None, *captured).await.is_err());
    assert_eq!(state.factories.load(Ordering::SeqCst), 0);
    drop(home);
}

#[tokio::test]
async fn sealed_success_and_failure_history_attach_after_shutdown_and_reread_offline_without_current_database_identity()
{
    for case in [SourceAcquisitionCase::Complete, SourceAcquisitionCase::Failed] {
        let fixture = LocalCaptureFixture::source_acquisition_for("shop", case).await;
        let mut snapshot = snapshots().requested;
        snapshot.identity = None;
        snapshot.runtime_populations = None;
        // An unidentified scan cannot assert that separate native populations share its database.
        let current = PopulationSnapshots::single(snapshot, &options()).unwrap();
        let (_home, workspace, state) = workspace(current, SourceShutdown::Quiescent);
        let captured = read("shop", &fixture).await;
        let expected = captured.original().unwrap().clone();
        let report = workspace.scan_with_captured_source_overhead("shop", &options(), None, *captured).await.unwrap();
        assert_eq!(state.active.load(Ordering::SeqCst), 0);
        assert_eq!(state.factories.load(Ordering::SeqCst), 1);
        let [RecordedSourceAcquisition::Captured { original }] =
            report.snapshot.operations.source_acquisitions.as_slice()
        else {
            panic!()
        };
        assert_eq!(original.as_ref(), &expected);
        let [HistoricalSourceOverhead::Available { report: view, .. }] = report.source_acquisitions.as_slice() else {
            panic!()
        };
        assert_eq!(view.evidence, expected.summary);
        if case == SourceAcquisitionCase::Failed {
            assert!(view.evidence.engine.is_none());
        }
        let id = report.snapshot.id.clone();
        let expected_view = serde_json::to_value(&report.source_acquisitions).unwrap();
        drop(fixture);
        let reopened = workspace.report_at("shop", &id).unwrap();
        assert_eq!(serde_json::to_value(&reopened.source_acquisitions).unwrap(), expected_view);
        assert_eq!(
            serde_json::to_value(culprit_engine::reports::display(&reopened)).unwrap()["source_acquisitions"],
            expected_view
        );
        assert_eq!(state.factories.load(Ordering::SeqCst), 1, "saved reanalysis starts no new provider");
        for contents in [
            culprit_engine::export::report_markdown(&reopened, &Default::default()),
            culprit_engine::export::report_html(&reopened, &Default::default()),
        ] {
            assert!(!contents.contains(&expected.manifest_sha256));
            assert!(!contents.contains(&expected.fingerprint));
            assert!(!contents.contains(&expected.summary.capture.observation_id));
        }
        let shared = culprit_checks::analyze(culprit_core::redact::redact_snapshot(&reopened.snapshot));
        assert!(matches!(shared.source_acquisitions.as_slice(), [HistoricalSourceOverhead::Unavailable { .. }]));
    }
}
