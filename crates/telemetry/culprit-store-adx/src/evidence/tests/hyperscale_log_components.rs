use super::*;
const FAMILIES: &[EvidenceFamily] = &[EvidenceFamily::OperationalSamples];
#[tokio::test]
async fn native_components_nulls_exact_values_and_original_coverage_round_trip_without_recovery() {
    for (source, records) in test_support::hyperscale_log_component_evidence_cases() {
        let fixture = Fixture::new(source.clone(), records.clone()).await;
        fixture.replies(vec![fixture.rows.clone()]);
        let capture =
            fixture.reader().read_capture(&fixture.range, FAMILIES, EvidenceReadBudget::default(), None).await.unwrap();
        test_support::assert_operational_capture(&capture, &source, &records);
        assert_eq!(capture.metrics().recovery_objects, 0);
    }
}
#[tokio::test]
async fn missing_native_component_retains_original_source_count_and_independent_warehouse_gap() {
    let (source, records) = test_support::hyperscale_log_component_evidence_cases().remove(0);
    let fixture = Fixture::new(source.clone(), records).await;
    fixture.replies(vec![fixture.rows.iter().skip(1).cloned().collect()]);
    let capture =
        fixture.reader().read_capture(&fixture.range, FAMILIES, EvidenceReadBudget::default(), None).await.unwrap();
    assert_eq!(capture.source(), &source);
    assert_eq!(capture.source().observation.snapshot().unwrap().sources[0].records, 2);
    assert!(!capture.is_read_complete());
    assert!(capture.gaps().iter().any(|gap| gap.reason == EvidenceGapReason::MissingRows));
}
#[tokio::test]
async fn changed_role_guid_counter_or_native_report_clock_cannot_pass_original_receipts() {
    let (source, records) = test_support::hyperscale_log_component_evidence_cases().remove(0);
    let fixture = Fixture::new(source, records).await;
    let details = fixture
        .catalog
        .get(TelemetryStream::OperationalSamples)
        .unwrap()
        .columns()
        .iter()
        .position(|column| column.name == "Details")
        .unwrap();
    for (field, value) in [
        ("component_id", json!("11111111-2222-3333-4444-555555555555")),
        ("role", json!(255)),
        ("catchup_distance_bytes", json!("1")),
        ("last_reported_time_without_offset", json!(null)),
    ] {
        let mut rows = fixture.rows.clone();
        let mut body: Value = serde_json::from_str(rows[0][details].as_str().unwrap()).unwrap();
        body[field] = value;
        rows[0][details] = json!(serde_json::to_string(&body).unwrap());
        fixture.replies(vec![rows]);
        assert!(
            fixture.reader().read_capture(&fixture.range, FAMILIES, EvidenceReadBudget::default(), None).await.is_err(),
            "{field}"
        );
    }
}
