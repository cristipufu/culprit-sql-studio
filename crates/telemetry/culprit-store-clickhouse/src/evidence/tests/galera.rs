use super::*;
#[tokio::test]
async fn galera_preserves_shared_original_and_independent_empty_partial_denied_native_sources() {
    for case in test_support::GaleraCase::ALL {
        let (source, records) = test_support::galera_evidence(case);
        let fixture = Fixture::new(source.clone(), records.clone()).await;
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
