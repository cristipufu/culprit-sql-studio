use super::*;
use culprit_collector::analytical::test_support::{ManagedInstanceResourceCase, managed_instance_resource_case};
use culprit_core::telemetry::SourceShutdown;
use culprit_engine::LocalCaptureAnalysisKind;
#[tokio::test]
async fn actual_local_cli_history_analysis_reuses_original_source_without_native_factory_or_file_mutation() {
    for case in ManagedInstanceResourceCase::ALL {
        let (source, rows) = managed_instance_resource_case(case);
        let mut fixture = LocalCaptureFixture::with_shutdown(source, rows, SourceShutdown::Quiescent).await;
        drop(fixture.outbox.take());
        let original = std::fs::read(fixture.capture_directory.join("manifest.json")).unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let server = server(&fixture, calls.clone());
        let (error, value) = analyze(
            &server,
            &fixture.manifest.capture.observation_id,
            LocalCaptureAnalysisKind::ManagedInstanceResources,
        )
        .await;
        assert!(!error, "{value}");
        assert_eq!(value["analysis"], "managed_instance_resources");
        assert_eq!(value["historical_context"], true);
        assert_eq!(value["managed_instance_resources"]["from"], "2020-01-01T06:00:00Z");
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert_eq!(fixture.manifest.source_queries, 0);
        assert_eq!(std::fs::read(fixture.capture_directory.join("manifest.json")).unwrap(), original);
    }
}
