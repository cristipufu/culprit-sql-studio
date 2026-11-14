use super::*;

#[tokio::test]
async fn activity_readback_preserves_exact_originals_unknowns_empty_populations_and_native_coverage() {
    for (source, records) in test_support::activity_evidence_cases() {
        let fixture =
            Fixture::new_for_stream(source.clone(), records.clone(), TelemetryStream::ActivityObservations).await;
        fixture.replies(fixture.rows.iter().map(|row| vec![row.clone()]).collect());
        let capture = fixture
            .reader
            .read_capture(
                &fixture.range,
                &[EvidenceFamily::ActivityObservations],
                EvidenceReadBudget { page_rows: 1, ..Default::default() },
            )
            .await
            .unwrap();
        test_support::assert_activity_capture(&capture, &source, &records);
        assert_eq!(capture.metrics().pages as usize, records.len());
        assert_eq!(fixture.catalog.get(TelemetryStream::ActivityObservations).unwrap().table(), "CulpritActivityV1");
        assert!(matches!(*fixture.recovery.fault.lock().unwrap(), Fault::Stall));
        if let Some(first) = fixture.rows.first() {
            let schema = fixture.catalog.get(TelemetryStream::ActivityObservations).unwrap();
            let column = schema.columns().iter().position(|column| column.name == "ActivityFacts").unwrap();
            for (pointer, value) in [
                ("/statement_identity/qualifiers/database_id", json!("another-native-database")),
                ("/open_transactions", json!(0)),
            ] {
                let mut changed = first.clone();
                let mut facts: Value = serde_json::from_str(changed[column].as_str().unwrap()).unwrap();
                *facts.pointer_mut(pointer).unwrap() = value;
                changed[column] = json!(serde_json::to_string(&facts).unwrap());
                fixture.replies(vec![vec![changed]]);
                let error = fixture
                    .reader
                    .read_capture(
                        &fixture.range,
                        &[EvidenceFamily::ActivityObservations],
                        EvidenceReadBudget::default(),
                    )
                    .await
                    .unwrap_err();
                assert_eq!(error.detail, Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::Integrity)));
                assert!(matches!(*fixture.recovery.fault.lock().unwrap(), Fault::Stall));
            }
        }
    }
}
