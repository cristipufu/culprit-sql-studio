use super::*;

#[tokio::test]
async fn diagnostic_revisions_keep_private_targets_out_of_warehouse_rows_and_restore_exact_originals() {
    for (source, records) in test_support::diagnostic_evidence_cases() {
        let fixture = Fixture::new(source.clone(), records.clone()).await;
        let encoded = serde_json::to_string(&fixture.rows).unwrap();
        assert!(!encoded.contains("private-diagnostic") && !encoded.contains("/private/diagnostics"));
        let schema = fixture.catalog.get(TelemetryStream::OperationalSamples).unwrap();
        let details = schema.columns().iter().position(|column| column.name == "Details").unwrap();
        let native: Value = serde_json::from_str(fixture.rows[0][details].as_str().unwrap()).unwrap();
        assert!(native["episode"]["scope"].get("target").is_none());
        fixture.replies(vec![fixture.rows.clone()]);
        let captured = fixture
            .reader
            .read_capture(&fixture.range, &[EvidenceFamily::OperationalSamples], EvidenceReadBudget::default())
            .await
            .unwrap();
        test_support::assert_diagnostic_capture(&captured, &source, &records);
        assert!(matches!(*fixture.recovery.fault.lock().unwrap(), Fault::Stall));

        let mut injected = fixture.rows[0].clone();
        let mut changed = native;
        changed["episode"]["scope"]["target"] = serde_json::to_value(&source.target).unwrap();
        injected[details] = json!(serde_json::to_string(&changed).unwrap());
        fixture.replies(vec![vec![injected]]);
        let error = fixture
            .reader
            .read_capture(&fixture.range, &[EvidenceFamily::OperationalSamples], EvidenceReadBudget::default())
            .await
            .unwrap_err();
        assert_eq!(error.detail, Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::Integrity)));
        assert!(matches!(*fixture.recovery.fault.lock().unwrap(), Fault::Stall));
    }
}
