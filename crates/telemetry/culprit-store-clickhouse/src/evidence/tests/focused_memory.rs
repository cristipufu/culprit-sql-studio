use super::*;
use std::sync::atomic::Ordering;

const FAMILY: &[EvidenceFamily] = &[EvidenceFamily::OperationalSamples];

#[tokio::test]
async fn focused_grant_round_trip_keeps_exact_original_context_and_publication_omits_private_owners() {
    let (source, records) = test_support::focused_request_memory_evidence();
    let fixture = Fixture::new(source.clone(), records.clone()).await;
    let before = fixture.recovery.object_reads.load(Ordering::SeqCst);
    fixture.replies(vec![fixture.rows.clone()]);
    let capture =
        fixture.reader.read_capture(&fixture.range, FAMILY, EvidenceReadBudget::default(), None).await.unwrap();
    test_support::assert_operational_capture(&capture, &source, &records);
    assert_eq!(fixture.recovery.object_reads.load(Ordering::SeqCst), before);
    let SourceObservation::FocusedRequestMemory { result, .. } = &source.observation else { panic!() };
    let schema = fixture.catalog.get(TelemetryStream::OperationalSamples).unwrap();
    let column = schema.columns().iter().position(|column| column.name == "FocusKey").unwrap();
    assert_eq!(fixture.rows[0][column], result.focus.key().unwrap());
    let schema = fixture.catalog.publication();
    let column = schema.columns().iter().position(|column| column.name == "SourceDescription").unwrap();
    let public: Value = serde_json::from_str(fixture.publication[column].as_str().unwrap()).unwrap();
    assert_eq!(public["observation"]["kind"], "focused-request-memory-context");
    assert_eq!(public["observation"]["data"]["focus_key"], result.focus.key().unwrap());
    let encoded = serde_json::to_string(&public).unwrap();
    assert!(!encoded.contains("private-memory") && !encoded.contains("private_memory"));
    assert!(public["observation"]["data"].get("focus").is_none());
    assert!(public["observation"]["data"].get("original").is_none());
}

#[tokio::test]
async fn another_valid_focus_key_cannot_replace_the_original_grant_receipt_even_with_recovery_enabled() {
    let (source, records) = test_support::focused_request_memory_evidence();
    let fixture = Fixture::new(source, records).await;
    let before = fixture.recovery.object_reads.load(Ordering::SeqCst);
    *fixture.recovery.fault.lock().unwrap() = Fault::None;
    let schema = fixture.catalog.get(TelemetryStream::OperationalSamples).unwrap();
    let column = schema.columns().iter().position(|column| column.name == "FocusKey").unwrap();
    let mut rows = fixture.rows.clone();
    rows[0][column] = json!(digest(b"another structurally valid focused request"));
    fixture.replies(vec![rows]);
    let error = fixture
        .reader
        .read_capture(
            &fixture.range,
            FAMILY,
            EvidenceReadBudget { recovery_payloads: Some(RecoveryPayloadBudget::default()), ..Default::default() },
            Some(fixture.recovery.as_ref()),
        )
        .await
        .unwrap_err();
    assert_eq!(error.detail, Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::Integrity)));
    assert_eq!(fixture.recovery.object_reads.load(Ordering::SeqCst), before);
}
