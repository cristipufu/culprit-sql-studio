use super::*;
#[tokio::test]
async fn local_change_feed_reopens_the_original_outbox_and_explains_without_a_native_source_or_event_dedupe() {
    for case in ChangeFeedCase::ALL {
        let (source, rows) = change_feed_evidence(case);
        let fixture = LocalCaptureFixture::snapshot(source.clone(), rows).await;
        let result = analyze(
            &reader(&fixture, CompletedCaptureBudget::default()),
            &fixture.manifest.capture.observation_id,
            LocalCaptureAnalysisKind::ChangeFeed,
        )
        .await
        .unwrap();
        assert_eq!(result.original().read().description, source);
        assert_eq!(result.original().read().origin.capture.capture, fixture.manifest.capture);
        assert_eq!(fixture.manifest.source_queries, 0);
        let LocalSnapshotReport::ChangeFeed(report) = result.report() else {
            panic!("typed original change-feed report")
        };
        assert!(report.limitations.contains(&culprit_core::t!("view.change_feed.limit.delivery")));
        assert_eq!(report.sources, source.observation.snapshot().unwrap().sources);
    }
}
