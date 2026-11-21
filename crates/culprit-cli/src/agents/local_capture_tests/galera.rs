use super::*;
use culprit_collector::analytical::test_support::{GaleraCase, galera_evidence};
use culprit_engine::LocalCaptureAnalysisKind;
#[tokio::test]
async fn actual_local_mcp_galera_route_keeps_native_values_and_authenticated_references_without_original_proof_exposure()
 {
    for case in GaleraCase::ALL {
        let (source, records) = galera_evidence(case);
        let original = source.observation.snapshot().unwrap().clone();
        let member_key = records.iter().find_map(|record| match record {
            TelemetryRecord::Operational(row) => Some(row.key.clone()),
            _ => None,
        });
        let mut fixture = LocalCaptureFixture::snapshot(source, records.clone()).await;
        drop(fixture.outbox.take());
        let calls = Arc::new(AtomicUsize::new(0));
        let server = server(&fixture, calls.clone());
        let (error, value) =
            analyze(&server, &fixture.manifest.capture.observation_id, LocalCaptureAnalysisKind::Galera).await;
        assert!(!error, "{case:?}");
        assert_eq!(value["analysis"], "galera");
        assert_eq!(value["historical_context"], true);
        assert_eq!(value["independent_shared_scope"], true);
        let report = &value["galera"];
        assert_eq!(report["native_rows"], original.records);
        assert_eq!(report["observed_rows"], records.len());
        assert_eq!(report["sources"].as_array().unwrap().len(), 1);
        if let Some(key) = member_key {
            let source = fixture.manifest.description.as_ref().unwrap();
            let scope = source.captured_range().unwrap().scope_id;
            let expected = digest(
                format!(
                    "local-snapshot-entry:{}:{}:{}:{}",
                    manifest_digest(&fixture),
                    scope,
                    fixture.manifest.capture.observation_id,
                    key
                )
                .as_bytes(),
            );
            assert_eq!(report["entries"][0]["reference"], expected);
        }
        for private in [
            "aaaaaaaa-0000-4000-8000-000000000001",
            "bbbbbbbb-0000-4000-8000-000000000002",
            "private-galera-process-canary",
            "manifest_sha256",
            "source_sha256",
            "qualifiers",
            "view-original",
        ] {
            assert!(!value.to_string().contains(private), "{private}");
        }
        if case == GaleraCase::Queued {
            assert_eq!(
                report["entries"][0]["groups"][1]["fields"][0]["value"],
                culprit_core::format::digits("9007199254740993".into())
            );
        }
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert_eq!(
            analyze(&server, &fixture.manifest.capture.observation_id, LocalCaptureAnalysisKind::Galera).await.1,
            value
        );
    }
}
