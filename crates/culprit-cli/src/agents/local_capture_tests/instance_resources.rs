use super::*;
use culprit_collector::analytical::test_support::{InstanceResourceCase, instance_resource_evidence};
use culprit_engine::LocalCaptureAnalysisKind;

#[tokio::test]
async fn exact_shared_instance_reports_reach_mcp_with_bounded_fields_original_clocks_and_no_private_scope() {
    for case in InstanceResourceCase::ALL {
        let (source, rows) = instance_resource_evidence(case);
        let native = source.observation.snapshot().unwrap().clone();
        let mut fixture = LocalCaptureFixture::snapshot(source, rows).await;
        drop(fixture.outbox.take());
        let calls = Arc::new(AtomicUsize::new(0));
        let server = server(&fixture, calls.clone());
        let (error, value) =
            analyze(&server, &fixture.manifest.capture.observation_id, LocalCaptureAnalysisKind::InstanceResources)
                .await;
        assert!(!error, "{case:?}:{value}");
        assert_eq!(value["independent_shared_scope"], true);
        assert_eq!(value["historical_context"], true);
        assert_eq!(value["read_complete"], true);
        assert_eq!(value["original_read_bounds"], json!({"from":native.observed_from,"until":native.observed_until}));
        let report = &value["instance_resources"];
        assert_eq!(report["source_state"], json!(native.sources[0].state));
        for group in report["groups"].as_array().unwrap() {
            assert!(group["fields"].as_array().unwrap().len() <= 5);
        }
        assert!(report["groups"].as_array().unwrap().len() <= 5);
        if case == InstanceResourceCase::Exact {
            assert!(value.to_string().contains("9007199254740993"));
        }
        for private in [
            "native-instance-namespace",
            "native-instance-a",
            "native-process-a",
            "manifest_sha256",
            "payload_sha256",
            "source_sha256",
            "qualifiers",
        ] {
            assert!(!value.to_string().contains(private), "{private}");
        }
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }
}
