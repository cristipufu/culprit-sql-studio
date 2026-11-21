use super::*;
#[tokio::test]
async fn actual_startup_history_route_retains_resolved_originals_without_a_database_or_warehouse() {
    for resolved in [false, true] {
        let mut fixture = LocalCaptureFixture::diagnostic_history(resolved).await;
        let bytes = fixture.original_bytes();
        drop(fixture.outbox.take());
        let calls = Arc::new(AtomicUsize::new(0));
        let server = server(&fixture, calls.clone());
        let (failed, value) =
            analyze(&server, &fixture.manifest.capture.observation_id, LocalCaptureAnalysisKind::IncidentHistory).await;
        assert!(!failed, "{value}");
        let report = &value["incident_history"];
        assert_eq!(report["provenance"], "authenticated-capture");
        assert_eq!(report["entries_available"], 1);
        assert_eq!(report["entries"][0]["lifecycle"], if resolved { "resolved" } else { "active" });
        assert_eq!(report["entries"][0]["original"]["capture"], json!(fixture.manifest.capture));
        assert_eq!(value["local_verification_bytes"], bytes);
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        let json = serde_json::to_string(&value).unwrap();
        assert!(!json.contains("private-activity-canary"));
        assert!(report["entries"][0].get("actions").is_none());
    }
}
