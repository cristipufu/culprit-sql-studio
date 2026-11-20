use super::*;
#[tokio::test]
async fn selected_table_memory_preserves_original_owner_bounds_nulls_and_partial_or_denied_coverage() {
    for case in test_support::TableMemoryCase::ALL {
        let (source, records) = test_support::table_memory_evidence(case);
        let fixture = Fixture::new(source.clone(), records.clone()).await;
        fixture.reply_families(&[EvidenceFamily::OperationalSamples], 1);
        let capture = fixture
            .reader()
            .read_capture(
                &fixture.range,
                &[EvidenceFamily::OperationalSamples],
                EvidenceReadBudget { page_rows: 1, ..budget() },
                None,
            )
            .await
            .unwrap();
        test_support::assert_operational_capture(&capture, &source, &records);
        assert!(matches!(*fixture.recovery.fault.lock().unwrap(), Fault::Stall));
    }
}
