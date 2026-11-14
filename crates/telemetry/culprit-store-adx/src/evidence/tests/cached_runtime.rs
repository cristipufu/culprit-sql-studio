use super::*;

#[tokio::test]
async fn completed_native_spill_pages_keep_exact_cache_identity_zero_and_unknown_without_recovery_reads() {
    for (source, records) in test_support::cached_runtime_evidence_cases() {
        let fixture = Fixture::new(source.clone(), records.clone()).await;
        fixture.reply_families(&[EvidenceFamily::OperationalSamples], 250);
        let captured = fixture
            .reader()
            .read_capture(&fixture.range, &[EvidenceFamily::OperationalSamples], budget())
            .await
            .unwrap();
        assert!(captured.is_read_complete(), "{:?}", captured.gaps());
        assert_eq!(captured.records().len(), 1);
        let expected = ExportedRecord::new(&source, captured.capture(), records[0].clone()).unwrap();
        assert_eq!(captured.records()[0].row(), &expected);
        assert!(matches!(*fixture.recovery.fault.lock().unwrap(), Fault::Stall));
    }
}

#[tokio::test]
async fn a_changed_spill_page_distribution_cannot_pass_the_committed_native_projection() {
    let (source, records) = test_support::cached_runtime_evidence_cases().pop().unwrap();
    let fixture = Fixture::new(source, records).await;
    let schema = fixture.catalog.get(TelemetryStream::OperationalSamples).unwrap();
    let column = schema.columns().iter().position(|column| column.name == "Details").unwrap();
    let mut rows = fixture.rows_for(EvidenceFamily::OperationalSamples);
    let mut native: Value = serde_json::from_str(rows[0][column].as_str().unwrap()).unwrap();
    native["counters"]["spilled-pages"]["total"] = json!("0");
    rows[0][column] = json!(serde_json::to_string(&native).unwrap());
    fixture.replies(vec![rows]);
    let error = fixture
        .reader()
        .read_capture(&fixture.range, &[EvidenceFamily::OperationalSamples], budget())
        .await
        .unwrap_err();
    assert_eq!(error.detail, Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::Integrity)));
}
