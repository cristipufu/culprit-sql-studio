use super::*;
use culprit_collector::analytical::test_support::SourceAcquisitionCase;
#[tokio::test]
async fn real_local_metadata_tool_inspects_success_failed_no_source_and_pending_allocation_without_native_authentication()
 {
    for case in [SourceAcquisitionCase::Complete, SourceAcquisitionCase::Failed] {
        let mut fixture = LocalCaptureFixture::source_acquisition(case).await;
        let native = Arc::new(AtomicUsize::new(0));
        let server = server(&fixture, native.clone());
        let (error, value) =
            analyze(&server, &fixture.manifest.capture.observation_id, LocalCaptureAnalysisKind::SourceOverhead).await;
        assert!(!error, "{value}");
        assert_eq!(value["seal"], "sealed");
        let evidence = &value["source_overhead"]["report"]["evidence"];
        assert_eq!(evidence["source_queries"], 100);
        assert_eq!(evidence["read_metrics"]["templates"].as_array().unwrap().len(), 32);
        if case == SourceAcquisitionCase::Failed {
            assert!(evidence["engine"].is_null());
        }
        assert!(evidence["read_metrics"]["templates"].as_array().unwrap().iter().all(
            |metric| metric["statement"]["state"] == "unavailable" && metric["statement"]["reason"] == "restricted"
        ));
        for canary in [
            "overhead_preview_canary",
            "native-cost-reader-db-canary",
            "native-cost-reader-host-canary",
            "native-cost-reader-machine-canary",
            "native-cost-reader-instance-canary",
            "native-cost-reader-process-canary",
        ] {
            assert!(!value.to_string().contains(canary), "{canary}");
        }
        assert!(value.get("manifest_sha256").is_none());
        assert_eq!(native.load(Ordering::SeqCst), 0);
        if case == SourceAcquisitionCase::Complete {
            let request = CollectionRequest {
                subject: CollectionSubject::Database,
                selection: CollectionSelection::Activity,
                budget: Default::default(),
            };
            let lease = fixture.outbox.as_mut().unwrap().reserve("local-evidence", &request).await.unwrap();
            let (error, pending) =
                analyze(&server, &lease.identity().observation_id, LocalCaptureAnalysisKind::SourceOverhead).await;
            assert!(!error);
            assert_eq!(pending["source_overhead"]["kind"], "unavailable");
            assert!(pending["seal"].is_null());
            assert!(pending["source_overhead"].get("report").is_none());
            lease.finish().await.unwrap();
        }
    }
}
#[tokio::test]
async fn metadata_tool_rejects_tampered_failed_manifest_and_cannot_relabel_another_saved_connections_attempt() {
    let fixture = LocalCaptureFixture::source_acquisition(SourceAcquisitionCase::Failed).await;
    let native = Arc::new(AtomicUsize::new(0));
    let server = server(&fixture, native.clone());
    let mut changed = fixture.manifest.clone();
    changed.source_queries += 1;
    std::fs::write(fixture.capture_directory.join("manifest.json"), serde_json::to_vec(&changed).unwrap()).unwrap();
    let (error, _) =
        analyze(&server, &fixture.manifest.capture.observation_id, LocalCaptureAnalysisKind::SourceOverhead).await;
    assert!(error);
    assert_eq!(native.load(Ordering::SeqCst), 0);
}
