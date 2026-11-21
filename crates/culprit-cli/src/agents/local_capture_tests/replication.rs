use super::*;
use culprit_collector::analytical::test_support::{ReplicationCase, replication_evidence};
use culprit_engine::LocalCaptureAnalysisKind;
#[tokio::test]
async fn actual_local_replication_mcp_keeps_original_role_topology_and_caveats_without_source_or_native_proof() {
    for case in [
        ReplicationCase::MssqlHyperscale,
        ReplicationCase::MssqlHyperscaleNulls,
        ReplicationCase::MssqlHyperscaleUnavailable,
        ReplicationCase::MssqlHyperscalePartial,
    ] {
        let (source, records) = replication_evidence(case);
        let native = source.observation.snapshot().unwrap().clone();
        let mut fixture = LocalCaptureFixture::snapshot(source.clone(), records.clone()).await;
        let before = std::fs::read(fixture.capture_directory.join("manifest.json")).unwrap();
        drop(fixture.outbox.take());
        let calls = Arc::new(AtomicUsize::new(0));
        let server = server(&fixture, calls.clone());
        let (error, value) =
            analyze(&server, &fixture.manifest.capture.observation_id, LocalCaptureAnalysisKind::Replication).await;
        assert!(!error, "{value}");
        assert_eq!(value["analysis"], "replication");
        assert_eq!(value["historical_context"], true);
        let view = &value["replication"];
        assert_eq!(view["native_rows"], native.records);
        assert_eq!(view["state_rows"], 0);
        assert_eq!(view["entries"].as_array().unwrap().len(), records.len());
        for entry in view["entries"].as_array().unwrap() {
            assert!(entry["reference"].as_str().unwrap().len() == 64);
            assert_eq!(entry["investigation"], culprit_core::t!("view.replication.mssql.hyperscale.investigate"));
            assert_eq!(entry["verify"], culprit_core::t!("view.replication.mssql.hyperscale.verify"));
            assert!(entry["conditions"].as_array().unwrap().is_empty());
        }
        assert!(
            view["limitations"]
                .as_array()
                .unwrap()
                .contains(&json!(culprit_core::t!("view.replication.mssql.hyperscale.limit")))
        );
        for key in ["manifest_sha256", "source_scope_id", "payload_sha256", "qualifiers", "original-native-process"] {
            assert!(!value.to_string().contains(key), "{key}");
        }
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert_eq!(std::fs::read(fixture.capture_directory.join("manifest.json")).unwrap(), before);
        assert_eq!(
            analyze(&server, &fixture.manifest.capture.observation_id, LocalCaptureAnalysisKind::Replication).await.1,
            value
        );
    }
}
