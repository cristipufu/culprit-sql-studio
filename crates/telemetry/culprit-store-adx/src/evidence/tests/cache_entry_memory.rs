use super::*;
use std::sync::atomic::Ordering;

const FAMILY: &[EvidenceFamily] = &[EvidenceFamily::OperationalSamples];

fn cache_budget() -> EvidenceReadBudget {
    EvidenceReadBudget { timeout_ms: 1000, ..Default::default() }
}

#[tokio::test]
async fn selected_cache_objects_round_trip_once_with_exact_statements_counts_unknowns_and_coverage() {
    for (_, source, records) in test_support::cache_entry_memory_evidence_cases() {
        let fixture = Fixture::new(source.clone(), records.clone()).await;
        let before = fixture.recovery.object_reads.load(Ordering::SeqCst);
        fixture.replies(fixture.rows.iter().map(|row| vec![row.clone()]).collect());
        let capture = fixture
            .reader()
            .read_capture(&fixture.range, FAMILY, EvidenceReadBudget { page_rows: 1, ..cache_budget() }, None)
            .await
            .unwrap();
        test_support::assert_cache_entry_memory_capture(&capture, &source, &records);
        assert_eq!(capture.metrics().pages as usize, records.len());
        assert_eq!(fixture.recovery.object_reads.load(Ordering::SeqCst), before);
        let SourceObservation::CacheEntryMemory { focus, .. } = &source.observation else { panic!() };
        let schema = fixture.catalog.get(TelemetryStream::OperationalSamples).unwrap();
        let focus_column = schema.columns().iter().position(|column| column.name == "FocusKey").unwrap();
        for row in &fixture.rows {
            assert_eq!(row[focus_column], focus.key().unwrap());
        }
        let schema = fixture.catalog.publication();
        let description = schema.columns().iter().position(|column| column.name == "SourceDescription").unwrap();
        let public: Value = serde_json::from_str(fixture.publication[description].as_str().unwrap()).unwrap();
        assert_eq!(public["observation"]["data"]["focus_key"], focus.key().unwrap());
        assert!(public["observation"]["data"].get("focus").is_none());
        assert!(public["observation"]["data"].get("context").is_none());
        let json = serde_json::to_string(&public).unwrap();
        for private in ["fixture.invalid", "native-member-a", "native-process-generation-1", "0x0102", "0x0304"] {
            assert!(!json.contains(private), "{private}");
        }
    }
}

#[tokio::test]
async fn altered_native_entry_membership_counts_scope_or_indexed_sql_fail_proof_before_recovery() {
    let (source, records) = test_support::cache_entry_memory_evidence(test_support::CacheEntryMemoryCase::Complete);
    let fixture = Fixture::new(source, records).await;
    *fixture.recovery.fault.lock().unwrap() = Fault::None;
    let before = fixture.recovery.object_reads.load(Ordering::SeqCst);
    let schema = fixture.catalog.get(TelemetryStream::OperationalSamples).unwrap();
    let details_column = schema.columns().iter().position(|column| column.name == "Details").unwrap();
    for (column, pointer, value) in [
        ("Details", "/size_bytes", json!("9007199254740994")),
        ("Details", "/lookup_count", json!("41")),
        ("Details", "/reference_count", json!("1")),
        ("Details", "/cache_keys/0/value", json!("2")),
        ("Details", "/statements/0/context/identity/plan_generation", json!("2")),
        ("Details", "/statements/0/context/identity/statement_start_bytes", json!(2)),
        ("Details", "/statements/0/context/database_id", json!(8)),
        ("Details", "/statements/1/statement/sha256", json!(digest(b"foreign SQL"))),
        ("Details", "/statements/1/statement/pointer", json!("/record/data/fact/data/statements/0/statement")),
        ("FactKey", "", json!(digest(b"foreign entry point"))),
        ("NativeSource", "", json!("foreign cache source")),
        ("SourceId", "", json!(digest(b"foreign source member/epoch"))),
        ("FocusKey", "", json!(digest(b"other valid selection"))),
    ] {
        let mut rows = fixture.rows.clone();
        if column == "Details" {
            let mut details: Value = serde_json::from_str(rows[0][details_column].as_str().unwrap()).unwrap();
            *details.pointer_mut(pointer).unwrap() = value;
            rows[0][details_column] = json!(details.to_string());
        } else {
            let index = schema.columns().iter().position(|field| field.name == column).unwrap();
            rows[0][index] = value;
        }
        fixture.replies(vec![rows]);
        let error = fixture
            .reader()
            .read_capture(
                &fixture.range,
                FAMILY,
                EvidenceReadBudget { recovery_payloads: Some(RecoveryPayloadBudget::default()), ..cache_budget() },
                Some(fixture.recovery.as_ref()),
            )
            .await
            .unwrap_err();
        assert_eq!(
            error.detail,
            Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::Integrity)),
            "{column}{pointer}"
        );
        assert_eq!(fixture.recovery.object_reads.load(Ordering::SeqCst), before);
    }
}

#[tokio::test]
async fn eight_long_indexed_statements_hydrate_as_one_authenticated_whole_cache_object() {
    let (source, records) =
        test_support::cache_entry_memory_evidence(test_support::CacheEntryMemoryCase::EightStatements);
    let fixture = Fixture::new(source.clone(), records.clone()).await;
    let before = fixture.recovery.object_reads.load(Ordering::SeqCst);
    fixture.replies(vec![fixture.rows.clone()]);
    let preview = fixture.reader().read_capture(&fixture.range, FAMILY, cache_budget(), None).await.unwrap();
    assert!(!preview.is_read_complete());
    assert!(preview.records().is_empty(), "one partial statement must not expose a partial native cache object");
    assert!(preview.gaps().iter().any(|gap| gap.reason == EvidenceGapReason::DetailInRecovery));
    assert_eq!(fixture.recovery.object_reads.load(Ordering::SeqCst), before);
    assert_eq!(preview.source(), &source);
    *fixture.recovery.fault.lock().unwrap() = Fault::None;
    fixture.replies(vec![fixture.rows.clone()]);
    let restored = fixture
        .reader()
        .read_capture(
            &fixture.range,
            FAMILY,
            EvidenceReadBudget { recovery_payloads: Some(RecoveryPayloadBudget::default()), ..cache_budget() },
            Some(fixture.recovery.as_ref()),
        )
        .await
        .unwrap();
    test_support::assert_cache_entry_memory_capture(&restored, &source, &records);
    assert_eq!(restored.records().len(), 1);
    assert_eq!(restored.records()[0].row().record.definition_payloads().count(), MAX_CACHE_ENTRY_STATEMENTS);
    assert_eq!(restored.metrics().recovery_objects, 1);
    assert_eq!(restored.metrics().recovery_bytes, fixture.range.manifest().batches[0].bytes);
    assert_eq!(fixture.recovery.object_reads.load(Ordering::SeqCst), before + 1);
}
