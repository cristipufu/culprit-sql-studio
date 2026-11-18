use super::*;
use std::sync::atomic::Ordering;

const FAMILIES: &[EvidenceFamily] = &[EvidenceFamily::OperationalSamples];

fn column(fixture: &Fixture, name: &str) -> usize {
    fixture
        .catalog
        .get(TelemetryStream::OperationalSamples)
        .unwrap()
        .columns()
        .iter()
        .position(|column| column.name == name)
        .unwrap()
}

#[tokio::test]
async fn configured_rates_original_headers_and_unrelated_sources_round_trip_without_recovery_reads() {
    for (source, records) in test_support::configured_log_rate_evidence_cases() {
        let fixture = Fixture::new(source.clone(), records.clone()).await;
        let opened = fixture.recovery.object_reads.load(Ordering::SeqCst);
        fixture.replies(vec![fixture.rows.clone()]);
        let capture =
            fixture.reader.read_capture(&fixture.range, FAMILIES, EvidenceReadBudget::default(), None).await.unwrap();
        test_support::assert_operational_capture(&capture, &source, &records);
        assert_eq!(capture.source().observation.snapshot().unwrap().sources.len(), 2);
        assert_eq!(fixture.recovery.object_reads.load(Ordering::SeqCst), opened);
        assert_eq!(capture.metrics().recovery_objects, 0);
    }
}

#[tokio::test]
async fn missing_configuration_keeps_original_count_and_scope_and_never_synthesizes_zero() {
    let (source, records) = test_support::configured_log_rate_evidence_cases().remove(0);
    let fixture = Fixture::new(source.clone(), records).await;
    let opened = fixture.recovery.object_reads.load(Ordering::SeqCst);
    let details = column(&fixture, "Details");
    let rows = fixture
        .rows
        .iter()
        .filter(|row| {
            let value: Value = serde_json::from_str(row[details].as_str().unwrap()).unwrap();
            value.get("workload_group_bytes_per_second").is_none()
        })
        .cloned()
        .collect();
    fixture.replies(vec![rows]);
    let capture =
        fixture.reader.read_capture(&fixture.range, FAMILIES, EvidenceReadBudget::default(), None).await.unwrap();
    assert_eq!(capture.source(), &source);
    assert!(!capture.is_read_complete());
    assert!(capture.gaps().iter().any(|gap| gap.reason == EvidenceGapReason::MissingRows));
    assert_eq!(capture.records().len(), 1);
    assert_eq!(fixture.recovery.object_reads.load(Ordering::SeqCst), opened);
}

#[tokio::test]
async fn altered_configuration_rates_native_namespace_or_clock_fail_original_receipts_before_payload_recovery() {
    let (source, records) = test_support::configured_log_rate_evidence_cases().remove(0);
    let fixture = Fixture::new(source, records).await;
    *fixture.recovery.fault.lock().unwrap() = Fault::None;
    let opened = fixture.recovery.object_reads.load(Ordering::SeqCst);
    let details_column = column(&fixture, "Details");
    let selected = fixture
        .rows
        .iter()
        .position(|row| {
            let value: Value = serde_json::from_str(row[details_column].as_str().unwrap()).unwrap();
            value["workload_group_bytes_per_second"] == json!("9007199254740993")
        })
        .unwrap();
    for (field, replacement) in [
        ("workload_group_bytes_per_second", json!("9007199254740992")),
        ("resource_pool_bytes_per_second", json!(null)),
        ("instance_bytes_per_second", json!("0")),
        ("database_incarnation", json!("foreign-original-database")),
        ("observed_at", json!("2020-01-01T12:00:00.500000000Z")),
    ] {
        let mut rows = fixture.rows.clone();
        let mut details: Value = serde_json::from_str(rows[selected][details_column].as_str().unwrap()).unwrap();
        details[field] = replacement;
        rows[selected][details_column] = json!(serde_json::to_string(&details).unwrap());
        fixture.replies(vec![rows]);
        let error = fixture
            .reader
            .read_capture(
                &fixture.range,
                FAMILIES,
                EvidenceReadBudget { recovery_payloads: Some(RecoveryPayloadBudget::default()), ..Default::default() },
                Some(fixture.recovery.as_ref()),
            )
            .await
            .unwrap_err();
        assert_eq!(error.detail, Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::Integrity)), "{field}");
        assert_eq!(fixture.recovery.object_reads.load(Ordering::SeqCst), opened, "{field}");
    }
}
