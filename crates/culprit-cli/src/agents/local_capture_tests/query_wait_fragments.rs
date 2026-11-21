use super::*;
use culprit_collector::analytical::test_support::query_wait_fragment_evidence;
#[tokio::test]
async fn local_wait_mcp_keeps_exact_native_fragment_metrics_and_context_without_original_proof_or_provider_work() {
    let (mut source, records) = query_wait_fragment_evidence();
    let CollectionTarget::Database(database) = &mut source.target else { panic!() };
    // Retain original logical namespace as a privacy canary by constructing the fixture before any projection.
    database.location = culprit_core::DatabaseLocation::Server {
        host: "private-query-history-host.invalid".into(),
        port: 5432,
        instance: None,
    };
    // Definition identities depend on the source; leave them unchanged to exercise unresolved physical retention.
    let records = records
        .into_iter()
        .filter_map(|record| match record {
            TelemetryRecord::QueryWaits(mut wait) => {
                wait.key.references = StatementReferences::default();
                Some(TelemetryRecord::QueryWaits(wait))
            }
            _ => None,
        })
        .collect();
    let mut fixture = LocalCaptureFixture::query_history(source, records).await;
    let calls = Arc::new(AtomicUsize::new(0));
    let server = server(&fixture, calls.clone());
    drop(fixture.outbox.take());
    let (error, value) =
        analyze(&server, &fixture.manifest.capture.observation_id, LocalCaptureAnalysisKind::QueryWaitFragments).await;
    assert!(!error, "{value}");
    let view = &value["query_wait_fragments"];
    assert_eq!(view["selected_rows"], 2);
    assert_eq!(view["entries_presented"], 2);
    assert_eq!(view["historical_context"], true);
    assert_ne!(view["entries"][0]["reference"], view["entries"][1]["reference"]);
    for entry in view["entries"].as_array().unwrap() {
        assert!(valid_digest(entry["reference"].as_str().unwrap()));
        assert_eq!(entry["informational"], true);
        assert_eq!(entry["metrics"].as_array().unwrap().len(), 6);
    }
    let text = value.to_string();
    assert!(!text.contains("private-query-history-host.invalid"));
    crate::test_support::assert_no_json_keys(
        &value,
        &[
            "manifest_sha256",
            "source_sha256",
            "payload_sha256",
            "native_payload_sha256",
            "native_key",
            "query_sql_text",
            "references",
            "qualifiers",
        ],
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}
