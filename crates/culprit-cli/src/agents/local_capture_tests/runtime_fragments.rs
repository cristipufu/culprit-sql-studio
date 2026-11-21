use super::*;
use culprit_collector::analytical::test_support::query_runtime_fragment_evidence;
#[tokio::test]
async fn runtime_mcp_reuses_original_local_history_and_keeps_unknown_means_exact_counts_and_endpoint_units_without_sql_or_native_proofs()
 {
    let (source, records) = query_runtime_fragment_evidence(true);
    let mut fixture = LocalCaptureFixture::query_history(source, records).await;
    let calls = Arc::new(AtomicUsize::new(0));
    let server = server(&fixture, calls.clone());
    drop(fixture.outbox.take());
    let (error, value) =
        analyze(&server, &fixture.manifest.capture.observation_id, LocalCaptureAnalysisKind::RuntimeFragments).await;
    assert!(!error, "{value}");
    let report = &value["runtime_fragments"];
    assert_eq!(report["entries_presented"], 2);
    assert_eq!(report["entries"][0]["executions"], "2");
    assert_eq!(report["entries"][1]["executions"], "3");
    assert_eq!(
        report["entries"][0]["metrics"][0]["statistics"][0]["text"],
        culprit_core::t!("view.runtime_fragments.unknown")
    );
    assert!(report["entries"][0]["metrics"][0]["statistics"][1]["text"].as_str().unwrap().contains("9007199254740993"));
    for entry in report["entries"].as_array().unwrap() {
        assert!(valid_digest(entry["reference"].as_str().unwrap()));
    }
    assert!(!value.to_string().contains("fixture-private-literal"));
    crate::test_support::assert_no_json_keys(
        &value,
        &["manifest_sha256", "source_sha256", "payload_sha256", "qualifiers", "query_sql_text"],
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}
