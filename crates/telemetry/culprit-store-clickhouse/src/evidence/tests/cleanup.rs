use super::*;

const FAMILY: &[EvidenceFamily] = &[EvidenceFamily::OperationalSamples];

#[tokio::test]
async fn cleanup_points_preserve_exact_native_fields_scope_horizon_coverage_and_source_failures() {
    for (case, source, records) in test_support::cleanup_evidence_cases() {
        let fixture = Fixture::new(source.clone(), records.clone()).await;
        fixture.replies(fixture.rows.iter().map(|row| vec![row.clone()]).collect());
        let capture = fixture
            .reader
            .read_capture(&fixture.range, FAMILY, EvidenceReadBudget { page_rows: 1, ..Default::default() }, None)
            .await
            .unwrap();
        test_support::assert_cleanup_capture(&capture, &source, &records);
        assert_eq!(capture.metrics().pages as usize, records.len(), "{case:?}");
        assert!(matches!(*fixture.recovery.fault.lock().unwrap(), Fault::Stall));
    }
}

#[tokio::test]
async fn native_counter_clock_generation_or_horizon_changes_fail_the_original_cleanup_receipt() {
    for (case, kind, pointer, replacement) in test_support::cleanup_tamper_cases() {
        let (source, records) = test_support::cleanup_evidence(case);
        let fixture = Fixture::new(source, records).await;
        let schema = fixture.catalog.get(TelemetryStream::OperationalSamples).unwrap();
        let kind_column = schema.columns().iter().position(|column| column.name == "FactKind").unwrap();
        let details_column = schema.columns().iter().position(|column| column.name == "Details").unwrap();
        let mut rows = fixture.rows.clone();
        let row = rows.iter_mut().find(|row| row[kind_column].as_str() == Some(kind)).unwrap();
        let mut details: Value = serde_json::from_str(row[details_column].as_str().unwrap()).unwrap();
        *details.pointer_mut(pointer).unwrap() = replacement;
        row[details_column] = json!(serde_json::to_string(&details).unwrap());
        fixture.replies(vec![rows]);
        let error =
            fixture.reader.read_capture(&fixture.range, FAMILY, EvidenceReadBudget::default(), None).await.unwrap_err();
        assert_eq!(
            error.detail,
            Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::Integrity)),
            "{kind}{pointer}"
        );
    }
}
