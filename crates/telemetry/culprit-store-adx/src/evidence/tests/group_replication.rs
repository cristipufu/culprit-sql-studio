use super::*;
#[tokio::test]
async fn group_replication_preserves_shared_original_and_independent_empty_partial_denied_native_sources() {
    for case in test_support::GroupReplicationCase::ALL {
        let (source, records) = test_support::group_replication_evidence(case);
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
