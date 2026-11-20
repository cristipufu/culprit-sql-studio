use super::*;
const FAMILY: &[EvidenceFamily] = &[EvidenceFamily::OperationalSamples];
#[tokio::test]
async fn original_retained_history_counts_and_unavailable_states_survive_the_physical_codec() {
    for case in test_support::SuspectPageHistoryCase::ALL {
        let (source, rows) = test_support::suspect_page_history_evidence(case);
        let fixture = Fixture::new(source.clone(), rows.clone()).await;
        fixture.replies(fixture.rows.iter().map(|row| vec![row.clone()]).collect());
        let capture = fixture
            .reader
            .read_capture(&fixture.range, FAMILY, EvidenceReadBudget { page_rows: 1, ..Default::default() }, None)
            .await
            .unwrap();
        test_support::assert_suspect_page_history_capture(&capture, &source, &rows);
        assert!(matches!(*fixture.recovery.fault.lock().unwrap(), Fault::Stall));
    }
}
#[tokio::test]
async fn changed_retained_count_or_current_native_id_cannot_reuse_an_original_receipt() {
    for (pointer, value) in [
        ("/retained_rows", json!("5")),
        ("/native_database_id", json!("8")),
        ("/observed_at", json!("2020-01-01T12:00:00.600000000Z")),
    ] {
        let (source, rows) = test_support::suspect_page_history_evidence(test_support::SuspectPageHistoryCase::Active);
        let fixture = Fixture::new(source, rows).await;
        let schema = fixture.catalog.get(TelemetryStream::OperationalSamples).unwrap();
        let column = schema.columns().iter().position(|column| column.name == "Details").unwrap();
        let mut rows = fixture.rows.clone();
        let mut details: Value = serde_json::from_str(rows[0][column].as_str().unwrap()).unwrap();
        *details.pointer_mut(pointer).unwrap() = value;
        rows[0][column] = json!(serde_json::to_string(&details).unwrap());
        fixture.replies(vec![rows]);
        let error =
            fixture.reader.read_capture(&fixture.range, FAMILY, EvidenceReadBudget::default(), None).await.unwrap_err();
        assert_eq!(error.detail, Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::Integrity)), "{pointer}");
    }
}
