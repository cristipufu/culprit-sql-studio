use super::*;
use culprit_collector::analytical::test_support::query_runtime_fragment_evidence;
#[tokio::test]
async fn local_runtime_and_wait_views_select_the_same_original_history_without_losing_unknown_means_or_native_endpoints()
 {
    let (source, records) = query_runtime_fragment_evidence(true);
    let fixture = LocalCaptureFixture::query_history(source.clone(), records).await;
    let reader =
        OutboxCaptureReader::delivery(fixture.outbox.as_ref().unwrap().delivery(), CompletedCaptureBudget::default())
            .unwrap();
    let guard = OperationGuard::new(reader.timeout()).unwrap();
    let result = reader
        .analyze(LocalCaptureRequest {
            connection: "local-evidence",
            observation_id: &fixture.manifest.capture.observation_id,
            analysis: LocalCaptureAnalysisKind::RuntimeFragments,
            operation: guard.handle(),
        })
        .await
        .unwrap();
    let LocalCapturedAnalysis::RuntimeFragments(value) = result else { panic!() };
    assert!(value.read().is_valid());
    assert_eq!(value.read().stream(), Some(TelemetryStream::QueryRuntime));
    assert_eq!(value.read().rows.len(), 2);
    assert_eq!(value.read().description, source);
    assert_eq!(value.bytes_read(), fixture.original_bytes());
    assert_eq!(value.report().entries[0].executions, "2");
    assert_eq!(
        value.report().entries[0].metrics[0].statistics[0].text.as_deref(),
        Some(culprit_core::t!("view.runtime_fragments.unknown").as_str())
    );
    assert_eq!(
        serde_json::to_value(value.report()).unwrap(),
        serde_json::to_value(culprit_checks::query_runtime_fragments::explain(value.read()).unwrap()).unwrap()
    );
}
