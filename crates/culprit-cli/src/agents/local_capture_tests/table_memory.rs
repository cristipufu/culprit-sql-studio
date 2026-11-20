use super::*;
use culprit_collector::analytical::test_support::{TableMemoryCase, table_memory_evidence};
use culprit_engine::LocalCaptureAnalysisKind;
#[tokio::test]
async fn actual_local_mcp_table_memory_route_preserves_canonical_values_and_qualification_without_source_work() {
    for case in TableMemoryCase::ALL {
        let (source, records) = table_memory_evidence(case);
        let SourceObservation::TableMemory { target, metadata, .. } = &source.observation else { panic!() };
        let target = target.clone();
        let metadata = metadata.clone();
        let native = source.observation.snapshot().unwrap().clone();
        let mut fixture = LocalCaptureFixture::snapshot(source, records.clone()).await;
        drop(fixture.outbox.take());
        let calls = Arc::new(AtomicUsize::new(0));
        let server = server(&fixture, calls.clone());
        let (error, value) =
            analyze(&server, &fixture.manifest.capture.observation_id, LocalCaptureAnalysisKind::TableMemory).await;
        assert!(!error, "{case:?}: {value}");
        assert_eq!(value["historical_context"], true);
        assert_eq!(value["native_scope_qualified"], true);
        assert_eq!(value["analysis"], "table_memory");
        assert_eq!(value["original_read_bounds"], json!({"from":native.observed_from,"until":native.observed_until}));
        let report = &value["table_memory"];
        assert_eq!(report["source_state"], json!(native.sources[0].state));
        assert_eq!(report["native_rows"], native.records);
        assert_eq!(report["observed_rows"], records.len());
        assert_eq!(
            report["selected_target"],
            json!({"database_id":target.database_id,"object_id":target.object_id,"schema":metadata.schema,"table":metadata.name})
        );
        assert_eq!(report["native_availability"]["dmv_permission"], json!(metadata.dmv_permission));
        assert_eq!(report["read_complete"], true);
        let entries = report["entries"].as_array().unwrap();
        assert_eq!(entries.len(), records.len());
        if let Some(entry) = entries.first() {
            let origin = DiagnosticCaptureRef {
                capture: fixture.manifest.capture.clone(),
                manifest_sha256: manifest_digest(&fixture),
                source_scope_id: fixture.manifest.description.as_ref().unwrap().captured_range().unwrap().scope_id,
            };
            let expected = digest(
                format!(
                    "local-snapshot-entry:{}:{}:{}:{}",
                    origin.manifest_sha256,
                    origin.source_scope_id,
                    origin.capture.observation_id,
                    target.key().unwrap()
                )
                .as_bytes(),
            );
            assert_eq!(entry["reference"], expected);
            assert_eq!(entry["metrics"].as_array().unwrap().len(), 4);
            if case == TableMemoryCase::Exact {
                assert_eq!(entry["metrics"][0]["text"], culprit_core::format::digits("9007199254740993".into()));
            }
        }
        for private in [
            "private-machine-canary",
            "private-instance-canary",
            "private-process-and-database-canary",
            "manifest_sha256",
            "source_sha256",
            "payload_sha256",
            "qualifiers",
            "original.invalid",
        ] {
            assert!(!value.to_string().contains(private), "{case:?}: {private}");
        }
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert_eq!(
            analyze(&server, &fixture.manifest.capture.observation_id, LocalCaptureAnalysisKind::TableMemory).await.1,
            value
        );
    }
}
#[tokio::test]
async fn local_mcp_wrong_kind_connection_and_missing_table_memory_originals_never_open_a_provider() {
    let (source, records) = table_memory_evidence(TableMemoryCase::Exact);
    let mut fixture = LocalCaptureFixture::snapshot(source, records).await;
    let file = fixture
        .manifest
        .files
        .iter()
        .find(|file| file.stream == TelemetryStream::OperationalSamples)
        .unwrap()
        .file
        .clone();
    drop(fixture.outbox.take());
    let calls = Arc::new(AtomicUsize::new(0));
    let server = server(&fixture, calls.clone());
    let id = &fixture.manifest.capture.observation_id;
    assert!(analyze(&server, id, LocalCaptureAnalysisKind::ColumnstoreRowgroups).await.0);
    std::fs::remove_file(fixture.capture_directory.join(file)).unwrap();
    assert!(analyze(&server, id, LocalCaptureAnalysisKind::TableMemory).await.0);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

fn manifest_digest(fixture: &LocalCaptureFixture) -> String {
    digest(&std::fs::read(fixture.capture_directory.join("manifest.json")).unwrap())
}
