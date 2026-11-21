use super::*;
use culprit_collector::analytical::test_support::{PoolWaitCounterCase, pool_wait_counter_evidence};
#[tokio::test]
async fn actual_mcp_startup_pool_wait_analysis_uses_original_references_and_never_constructs_native_sources() {
    for case in PoolWaitCounterCase::ALL {
        let (source, rows) = pool_wait_counter_evidence(case);
        let mut fixture = LocalCaptureFixture::snapshot(source, rows).await;
        drop(fixture.outbox.take());
        let original = manifest_digest(&fixture);
        let calls = Arc::new(AtomicUsize::new(0));
        let server = server(&fixture, calls.clone());
        let (error, value) =
            analyze(&server, &fixture.manifest.capture.observation_id, LocalCaptureAnalysisKind::WaitCounters).await;
        assert!(!error, "{value}");
        assert_eq!(value["analysis"], "wait_counters");
        assert_eq!(value["historical_context"], true);
        let report = &value["wait_counters"];
        assert_eq!(report["read_complete"], true);
        for entry in report["entries"].as_array().unwrap() {
            assert_eq!(entry["scope"], "pool");
            assert_eq!(entry["reset"]["kind"], "unknown");
            assert_eq!(entry["reference"].as_str().unwrap().len(), 64);
        }
        if matches!(
            case,
            PoolWaitCounterCase::GeneralPurpose
                | PoolWaitCounterCase::BusinessCritical
                | PoolWaitCounterCase::NativePartial
        ) {
            assert_eq!(report["entries"][0]["wait_time_ms"], "9007199254740993");
        }
        if matches!(
            case,
            PoolWaitCounterCase::Hyperscale
                | PoolWaitCounterCase::UnknownTier
                | PoolWaitCounterCase::VisibilityUnknown
                | PoolWaitCounterCase::Denied
        ) {
            assert_eq!(report["sources"][0]["state"], "unavailable");
            assert_ne!(report["summary"], culprit_core::t!("view.wait_counters.summary.empty"));
        }
        for private in [
            "qualifiers",
            "logical_database_guid",
            "physical_database_guid",
            "manifest_sha256",
            "11111111-1111-1111-1111-111111111111",
        ] {
            assert!(!value.to_string().contains(private));
        }
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert_eq!(manifest_digest(&fixture), original);
        assert_eq!(fixture.manifest.source_queries, 0);
    }
}
