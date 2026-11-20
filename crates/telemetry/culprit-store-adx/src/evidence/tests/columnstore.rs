use super::*;
#[tokio::test]
async fn columnstore_configuration_replays_exact_native_ids_minutes_zero_and_unavailable_states() {
    let cases = test_support::columnstore_configuration_evidence_cases();
    assert_eq!(cases.len(), 6);
    for (source, records) in cases {
        let fixture = Fixture::new(source.clone(), records.clone()).await;
        fixture.reply_families(&[EvidenceFamily::OperationalSamples], 1);
        let capture = fixture
            .reader()
            .read_capture(&fixture.range, &[EvidenceFamily::OperationalSamples], budget(), None)
            .await
            .unwrap();
        test_support::assert_operational_capture(&capture, &source, &records);
        assert!(matches!(*fixture.recovery.fault.lock().unwrap(), Fault::Stall));
    }
}
