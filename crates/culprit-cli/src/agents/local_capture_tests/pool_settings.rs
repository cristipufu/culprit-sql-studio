use super::*;
use culprit_collector::analytical::test_support::{PoolSettingsCase, pool_settings_evidence};
use culprit_engine::LocalCaptureAnalysisKind;
#[tokio::test]
async fn actual_local_mcp_pool_analysis_keeps_original_settings_references_scope_and_unknowns_without_native_factory_work()
 {
    for case in PoolSettingsCase::ALL {
        let (source, rows) = pool_settings_evidence(case);
        let mut fixture = LocalCaptureFixture::snapshot(source, rows).await;
        drop(fixture.outbox.take());
        let original = std::fs::read(fixture.capture_directory.join("manifest.json")).unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let server = server(&fixture, calls.clone());
        let (error, value) =
            analyze(&server, &fixture.manifest.capture.observation_id, LocalCaptureAnalysisKind::PoolSettings).await;
        assert!(!error, "{value}");
        assert_eq!(value["analysis"], "pool_settings");
        assert_eq!(value["historical_context"], true);
        let report = &value["pool_settings"];
        assert_eq!(report["scope"], "pool");
        assert_eq!(report["member_database"], "recorded-member");
        if matches!(
            case,
            PoolSettingsCase::SingleDatabase
                | PoolSettingsCase::MembershipDenied
                | PoolSettingsCase::MembershipUnavailable
        ) {
            assert!(report["pool"].is_null());
        } else {
            assert_eq!(report["pool"], "recorded-elastic-pool");
        }
        for field in report["fields"].as_array().unwrap() {
            assert_eq!(field["reference"].as_str().unwrap().len(), 64);
        }
        if case == PoolSettingsCase::Exact {
            assert_eq!(report["fields"][0]["native_value"], "0");
            assert!(report["fields"][3]["value"].is_null());
        }
        if matches!(case, PoolSettingsCase::Denied | PoolSettingsCase::MissingLayout) {
            assert_eq!(report["source_state"], "unavailable");
            assert!(report["fields"].as_array().unwrap().is_empty());
        }
        for private in ["manifest_sha256", "source_scope_id", "logical_database_guid", "physical_database_guid"] {
            assert!(!value.to_string().contains(private), "{private}");
        }
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert_eq!(fixture.manifest.source_queries, 0);
        assert_eq!(std::fs::read(fixture.capture_directory.join("manifest.json")).unwrap(), original);
    }
}
