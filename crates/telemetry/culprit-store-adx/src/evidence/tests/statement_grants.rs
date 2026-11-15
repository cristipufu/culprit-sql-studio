use super::*;
use std::sync::atomic::Ordering;
const FAMILY: &[EvidenceFamily] = &[EvidenceFamily::OperationalSamples];

fn grant_budget() -> EvidenceReadBudget {
    EvidenceReadBudget { timeout_ms: 1000, ..Default::default() }
}

#[tokio::test]
async fn original_cached_grant_entries_round_trip_with_exact_context_counts_payloads_and_source_coverage() {
    for (_, source, records) in test_support::statement_grant_evidence_cases() {
        let fixture = Fixture::new(source.clone(), records.clone()).await;
        let before = fixture.recovery.object_reads.load(Ordering::SeqCst);
        fixture.replies(fixture.rows.iter().map(|row| vec![row.clone()]).collect());
        let capture = fixture
            .reader()
            .read_capture(&fixture.range, FAMILY, EvidenceReadBudget { page_rows: 1, ..grant_budget() }, None)
            .await
            .unwrap();
        test_support::assert_statement_grant_capture(&capture, &source, &records);
        assert_eq!(capture.metrics().pages as usize, records.len());
        assert_eq!(fixture.recovery.object_reads.load(Ordering::SeqCst), before);
        assert!(matches!(*fixture.recovery.fault.lock().unwrap(), Fault::Stall));
    }
}

#[tokio::test]
async fn altered_grant_identity_counters_payload_source_and_scope_fail_the_original_proof_without_payload_reads() {
    let (source, records) = test_support::statement_grant_evidence(test_support::StatementGrantCase::Complete);
    let fixture = Fixture::new(source, records).await;
    let before = fixture.recovery.object_reads.load(Ordering::SeqCst);
    let schema = fixture.catalog.get(TelemetryStream::OperationalSamples).unwrap();
    let details_index = schema.columns().iter().position(|column| column.name == "Details").unwrap();
    for (column, pointer, value) in [
        ("Details", "/context/identity/plan_generation", json!("2")),
        ("Details", "/context/identity/statement_start_bytes", json!(2)),
        ("Details", "/total_grant_kibibytes", json!("83886081")),
        ("Details", "/statement/sha256", json!(digest(b"foreign SQL"))),
        ("FactKey", "", json!(digest(b"foreign cache entry"))),
        ("NativeSource", "", json!("foreign-grant-source")),
        ("SourceId", "", json!(digest(b"other native member/epoch"))),
    ] {
        let mut rows = fixture.rows.clone();
        if column == "Details" {
            let mut details: Value = serde_json::from_str(rows[0][details_index].as_str().unwrap()).unwrap();
            *details.pointer_mut(pointer).unwrap() = value;
            rows[0][details_index] = json!(details.to_string());
        } else {
            let index = schema.columns().iter().position(|field| field.name == column).unwrap();
            rows[0][index] = value;
        }
        fixture.replies(vec![rows]);
        let error = fixture.reader().read_capture(&fixture.range, FAMILY, grant_budget(), None).await.unwrap_err();
        assert_eq!(
            error.detail,
            Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::Integrity)),
            "{column}{pointer}"
        );
        assert_eq!(fixture.recovery.object_reads.load(Ordering::SeqCst), before);
    }
}

#[tokio::test]
async fn a_complete_native_statement_beyond_the_utf8_preview_needs_explicit_bounded_recovery() {
    let (source, records) = test_support::statement_grant_evidence(test_support::StatementGrantCase::LargeStatement);
    let fixture = Fixture::new(source.clone(), records.clone()).await;
    let before = fixture.recovery.object_reads.load(Ordering::SeqCst);
    fixture.replies(vec![fixture.rows.clone()]);
    let preview = fixture.reader().read_capture(&fixture.range, FAMILY, grant_budget(), None).await.unwrap();
    assert!(!preview.is_read_complete());
    assert_eq!(preview.records().len(), 1);
    assert!(preview.gaps().iter().any(|gap| gap.reason == EvidenceGapReason::DetailInRecovery));
    assert_eq!(fixture.recovery.object_reads.load(Ordering::SeqCst), before);
    assert_eq!(preview.source(), &source);
    *fixture.recovery.fault.lock().unwrap() = Fault::None;
    fixture.replies(vec![fixture.rows.clone()]);
    let complete = fixture
        .reader()
        .read_capture(
            &fixture.range,
            FAMILY,
            EvidenceReadBudget { recovery_payloads: Some(RecoveryPayloadBudget::default()), ..grant_budget() },
            Some(fixture.recovery.as_ref()),
        )
        .await
        .unwrap();
    test_support::assert_statement_grant_capture(&complete, &source, &records);
    assert_eq!(complete.metrics().recovery_objects, 1);
    assert_eq!(complete.metrics().recovery_bytes, fixture.range.manifest().batches[0].bytes);
    assert_eq!(fixture.recovery.object_reads.load(Ordering::SeqCst), before + 1);
}
