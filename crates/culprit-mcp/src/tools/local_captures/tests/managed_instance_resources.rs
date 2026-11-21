use super::*;
use culprit_collector::{
    analytical::test_support::{LocalCaptureFixture, ManagedInstanceResourceCase, managed_instance_resource_case},
    outbox::CompletedCaptureBudget,
};
use culprit_core::telemetry::SourceShutdown;
use culprit_engine::OutboxCaptureReader;
#[tokio::test]
async fn selected_local_history_uses_canonical_values_and_preserves_all_three_clock_scopes() {
    for case in ManagedInstanceResourceCase::ALL {
        let (source, rows) = managed_instance_resource_case(case);
        let fixture = LocalCaptureFixture::with_shutdown(source, rows, SourceShutdown::Quiescent).await;
        let reader = OutboxCaptureReader::delivery(
            fixture.outbox.as_ref().unwrap().delivery(),
            CompletedCaptureBudget::default(),
        )
        .unwrap();
        let mut host = Host::new();
        host.server = host.server.clone().with_local_captures(Arc::new(reader));
        let args = json!({"connection":"local-evidence","observation_id":fixture.manifest.capture.observation_id,"analysis":"managed_instance_resources"});
        let result = host.server.handle("fixture", "analyze_capture", args.as_object().unwrap().clone()).await;
        assert_ne!(result.is_error, Some(true), "{case:?}");
        let value = body(&result);
        let report = &value["managed_instance_resources"];
        assert_eq!(value["historical_context"], true);
        assert_eq!(report["from"], "2020-01-01T06:00:00Z");
        assert_eq!(report["observed_at"], "2020-01-01T06:10:00Z");
        assert_eq!(value["collected_from"], fixture.manifest.started_at);
        assert_ne!(value["collected_from"], report["from"]);
        for private in [
            "current_reader",
            "private-current-machine",
            "private-current-instance",
            "private-current-process",
            "manifest_sha256",
            "payload_sha256",
        ] {
            assert!(!value.to_string().contains(private), "{private}");
        }
    }
}
