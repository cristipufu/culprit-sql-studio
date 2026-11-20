use super::*;
use culprit_collector::analytical::test_support::{SuspectPageHistoryCase, suspect_page_history_evidence};
#[tokio::test]
async fn original_suspect_history_analysis_preserves_nullable_and_unavailable_states_without_a_provider() {
    for case in SuspectPageHistoryCase::ALL {
        let (source, rows) = suspect_page_history_evidence(case);
        let fixture = LocalCaptureFixture::snapshot(source.clone(), rows).await;
        let reader = OutboxCaptureReader::delivery(
            fixture.outbox.as_ref().unwrap().delivery(),
            CompletedCaptureBudget::default(),
        )
        .unwrap();
        let guard = OperationGuard::new(reader.timeout()).unwrap();
        let result = reader
            .analyze(LocalCaptureRequest {
                connection: "local-evidence",
                observation_id: &fixture.manifest.capture.observation_id,
                analysis: LocalCaptureAnalysisKind::SuspectPages,
                operation: guard.handle(),
            })
            .await
            .unwrap();
        let LocalCapturedAnalysis::Snapshot(result) = result else { panic!() };
        let LocalSnapshotReport::SuspectPages(report) = result.report() else { panic!() };
        assert_eq!(result.original().read().description, source);
        assert!(result.original().read().is_valid());
        assert_eq!(
            report.native_source.as_ref().unwrap().state,
            source.observation.snapshot().unwrap().sources[0].state
        );
        if matches!(case, SuspectPageHistoryCase::Denied | SuspectPageHistoryCase::Unsupported) {
            assert!(report.retained_rows.is_none());
        }
        if case == SuspectPageHistoryCase::RepairedOnly {
            assert_eq!(report.retained_rows.as_deref(), Some("3"));
            assert_eq!(report.active_rows.as_deref(), Some("0"));
        }
    }
}
