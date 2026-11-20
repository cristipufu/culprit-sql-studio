use super::*;
#[tokio::test]
async fn selected_table_memory_preserves_original_owner_bounds_nulls_and_partial_or_denied_coverage() {
    for case in test_support::TableMemoryCase::ALL {
        let (source, records) = test_support::table_memory_evidence(case);
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
