use super::*;
use std::sync::atomic::Ordering;

const FAMILY: &[EvidenceFamily] = &[EvidenceFamily::OperationalSamples];

#[tokio::test]
async fn compilation_policy_requires_complete_authenticated_sql_and_rejects_changed_payload_proof() {
    let (source, records) = test_support::query_compilation_policy_evidence();
    let fixture = Fixture::new(source.clone(), records.clone()).await;
    let before = fixture.recovery.object_reads.load(Ordering::SeqCst);
    fixture.reply_families(FAMILY, 250);
    let preview = fixture.reader().read_capture(&fixture.range, FAMILY, budget(), None).await.unwrap();
    assert!(preview.records().is_empty());
    assert!(!preview.is_read_complete());
    assert_eq!(preview.gaps()[0].reason, EvidenceGapReason::DetailInRecovery);
    assert_eq!(fixture.recovery.object_reads.load(Ordering::SeqCst), before);

    *fixture.recovery.fault.lock().unwrap() = Fault::None;
    fixture.reply_families(FAMILY, 250);
    let options = EvidenceReadBudget { recovery_payloads: Some(RecoveryPayloadBudget::default()), ..budget() };
    let capture =
        fixture.reader().read_capture(&fixture.range, FAMILY, options, Some(fixture.recovery.as_ref())).await.unwrap();
    test_support::assert_operational_capture(&capture, &source, &records);
    assert_eq!(capture.records()[0].row().record, records[0]);
    assert_eq!(capture.metrics().recovery_objects, 1);
    assert!(!serde_json::to_string(&capture).unwrap().contains("private-compilation-value"));

    let schema = fixture.catalog.get(TelemetryStream::OperationalSamples).unwrap();
    let column = schema.columns().iter().position(|column| column.name == "Details").unwrap();
    let mut rows = fixture.rows_for(EvidenceFamily::OperationalSamples);
    let mut details: Value = serde_json::from_str(rows[0][column].as_str().unwrap()).unwrap();
    assert!(details["statement"].get("text").is_none());
    details["statement"]["sha256"] = json!(digest(b"different statement"));
    rows[0][column] = json!(details.to_string());
    fixture.replies(vec![rows]);
    let before = fixture.recovery.object_reads.load(Ordering::SeqCst);
    let error = fixture
        .reader()
        .read_capture(&fixture.range, FAMILY, options, Some(fixture.recovery.as_ref()))
        .await
        .unwrap_err();
    assert_eq!(error.detail, Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::Integrity)));
    assert_eq!(fixture.recovery.object_reads.load(Ordering::SeqCst), before);
}
