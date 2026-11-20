use super::*;
use culprit_collector::analytical::test_support::{TempdbMetadataMemoryCase, tempdb_metadata_memory_evidence};
use culprit_engine::LocalCaptureAnalysisKind;
use serde_json::Value;

#[tokio::test]
async fn metadata_local_action_preserves_disabled_unknown_and_partial_native_evidence_without_source_calls_or_private_proof()
 {
    for case in TempdbMetadataMemoryCase::ALL {
        let (source, mut records) = tempdb_metadata_memory_evidence(case);
        for record in &mut records {
            if let TelemetryRecord::Operational(row) = record {
                if let OperationalFact::CounterSet(sample) = &mut row.fact {
                    sample.properties.insert("private_unused_proof".into(), "private-metadata-proof-canary".into());
                    sample
                        .properties
                        .insert("private_unused_sql".into(), "SELECT 'private-metadata-sql-canary'".into());
                }
            }
        }
        let expected = source.observation.snapshot().unwrap().sources[0].clone();
        let mut fixture = LocalCaptureFixture::snapshot(source, records).await;
        drop(fixture.outbox.take());
        let calls = Arc::new(AtomicUsize::new(0));
        let server = server(&fixture, calls.clone());
        let (error, value) =
            analyze(&server, &fixture.manifest.capture.observation_id, LocalCaptureAnalysisKind::TempdbMetadataMemory)
                .await;
        assert!(!error, "{case:?}: {value}");
        assert_eq!(value["analysis"], "tempdb_metadata_memory");
        assert_eq!(value["historical_context"], true);
        assert_eq!(value["independent_shared_scope"], true);
        assert_eq!(value["native_scope_qualified"], true);
        assert_eq!(value["local_verification_bytes"], fixture.original_bytes());
        let report = &value["tempdb_metadata_memory"];
        assert_eq!(report["native_source"]["state"], json!(expected.state));
        assert_eq!(report["native_rows"], expected.records);
        assert_eq!(report["observed_rows"], expected.records);
        assert_eq!(report["read_complete"], true);
        assert_eq!(report["tempdb_created_without_offset"], "2020-01-01T14:00:00.003");
        assert_eq!(report["feature_observed_at"], "2020-01-01T12:00:00.2Z");
        if matches!(case, TempdbMetadataMemoryCase::Reserved | TempdbMetadataMemoryCase::NativePartial) {
            assert_eq!(report["metadata_enabled"], true);
            assert_eq!(report["entries"][0]["allocated_bytes"], "9007199254740993");
            assert_eq!(report["entries"][0]["used_bytes"], "9007199254740992");
            assert_eq!(report["entries"][0]["observed_at"], "2020-01-01T12:00:00.5Z");
            for field in ["explanation", "investigation", "verify"] {
                assert!(report["entries"][0][field].as_str().is_some_and(|text| !text.is_empty()));
            }
        } else {
            assert!(report["entries"].as_array().unwrap().is_empty());
            assert_eq!(
                report["metadata_enabled"],
                if case == TempdbMetadataMemoryCase::Disabled { json!(false) } else { Value::Null }
            );
        }
        let text = value.to_string();
        for private in [
            "private-metadata-proof-canary",
            "private-metadata-sql-canary",
            "manifest_sha256",
            "source_sha256",
            "payload_sha256",
            "qualifiers",
            "native-instance-a",
            "native-instance-namespace",
        ] {
            assert!(!text.contains(private), "{case:?}: {private}");
        }
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert_eq!(
            analyze(&server, &fixture.manifest.capture.observation_id, LocalCaptureAnalysisKind::TempdbMetadataMemory)
                .await
                .1,
            value
        );
    }
}

#[tokio::test]
async fn metadata_local_action_caps_five_consumers_without_changing_native_counts_or_exposing_omitted_context() {
    let (mut source, records) = tempdb_metadata_memory_evidence(TempdbMetadataMemoryCase::Reserved);
    let feature = records[0].clone();
    let TelemetryRecord::Operational(template) = &records[1] else { panic!() };
    let mut rows = vec![feature];
    for index in 0..7 {
        let mut row = template.clone();
        let OperationalFact::CounterSet(sample) = &mut row.fact else { panic!() };
        sample.identity.id = (42 + index).to_string();
        sample.properties.insert("memory_consumer_desc".into(), format!("consumer-{index}"));
        row.key = sample.key().unwrap();
        rows.push(TelemetryRecord::Operational(row));
    }
    rows.push(records[2].clone());
    let SourceObservation::Snapshot(header) = &mut source.observation else { panic!() };
    header.records = rows.len() as u32;
    header.sources[0] = StateSourceCoverage::observed(culprit_core::source_id::TEMPDB_METADATA_MEMORY.into(), 8, true);
    let mut fixture = LocalCaptureFixture::snapshot(source, rows).await;
    drop(fixture.outbox.take());
    let calls = Arc::new(AtomicUsize::new(0));
    let server = server(&fixture, calls.clone());
    let (error, value) =
        analyze(&server, &fixture.manifest.capture.observation_id, LocalCaptureAnalysisKind::TempdbMetadataMemory)
            .await;
    assert!(!error, "{value}");
    let report = &value["tempdb_metadata_memory"];
    assert_eq!(value["native_rows"], 9, "the full original also retains its independent native counter");
    assert_eq!(report["native_rows"], 8, "one feature point plus seven consumers");
    assert_eq!(report["observed_rows"], 8);
    assert_eq!(report["entries_available"], 7);
    assert_eq!(report["entries_presented"], 5);
    assert_eq!(report["entries_omitted"], 2);
    assert_eq!(report["entries_limited"], true);
    assert_eq!(report["entries"].as_array().unwrap().len(), 5);
    assert!(!value.to_string().contains("consumer-5"));
    assert!(!value.to_string().contains("consumer-6"));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}
