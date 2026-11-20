use super::*;
#[tokio::test]
async fn selected_full_text_populations_preserve_original_target_scope_native_states_nulls_and_coverage() {
    for case in test_support::FullTextPopulationCase::ALL {
        let (source, records) = test_support::full_text_population_evidence(case);
        let fixture =
            Fixture::new_for_stream(source.clone(), records.clone(), TelemetryStream::OperationalSamples).await;
        fixture.replies(fixture.rows.iter().map(|row| vec![row.clone()]).collect());
        let capture = fixture
            .reader
            .read_capture(
                &fixture.range,
                &[EvidenceFamily::OperationalSamples],
                EvidenceReadBudget { page_rows: 1, ..Default::default() },
                None,
            )
            .await
            .unwrap();
        test_support::assert_operational_capture(&capture, &source, &records);
        assert!(matches!(*fixture.recovery.fault.lock().unwrap(), Fault::Stall));
    }
}

#[tokio::test]
async fn a_changed_native_runtime_status_cannot_borrow_the_original_population_row_receipt() {
    let (source, records) =
        test_support::full_text_population_evidence(test_support::FullTextPopulationCase::Processing);
    let fixture = Fixture::new_for_stream(source.clone(), records.clone(), TelemetryStream::OperationalSamples).await;
    let schema = fixture.catalog.get(TelemetryStream::OperationalSamples).unwrap();
    let column = schema.columns().iter().position(|column| column.name == "Details").unwrap();
    let mut rows = fixture.rows.clone();
    let mut details: Value = serde_json::from_str(rows[0][column].as_str().unwrap()).unwrap();
    details["status"] = json!(11);
    rows[0][column] = json!(serde_json::to_string(&details).unwrap());
    fixture.replies(vec![rows]);
    let error = fixture
        .reader
        .read_capture(
            &fixture.range,
            &[EvidenceFamily::OperationalSamples],
            EvidenceReadBudget { page_rows: 1, ..Default::default() },
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(error.detail, Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::Integrity)));
}
