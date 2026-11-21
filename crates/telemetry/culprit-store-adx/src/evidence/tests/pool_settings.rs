use super::*;
use std::sync::atomic::Ordering;
const FAMILY: &[EvidenceFamily] = &[EvidenceFamily::SettingObservations];
#[tokio::test]
async fn original_pool_settings_use_shared_projection_nullable_values_and_native_membership_without_recovery_reads() {
    for case in test_support::PoolSettingsCase::ALL {
        let (source, records) = test_support::pool_settings_evidence(case);
        let fixture = Fixture::new(source.clone(), records.clone()).await;
        let opened = fixture.recovery.object_reads.load(Ordering::SeqCst);
        fixture.replies(fixture.rows.iter().map(|row| vec![row.clone()]).collect());
        let capture = fixture
            .reader()
            .read_capture(&fixture.range, FAMILY, EvidenceReadBudget { page_rows: 1, ..Default::default() }, None)
            .await
            .unwrap();
        test_support::assert_pool_settings_capture(&capture, &source, &records);
        assert_eq!(capture.metrics().selected_rows as usize, records.len());
        assert_eq!(capture.metrics().recovery_objects, 0);
        assert_eq!(fixture.recovery.object_reads.load(Ordering::SeqCst), opened);
    }
}
#[tokio::test]
async fn changed_pool_setting_projection_cannot_borrow_the_original_native_receipt_or_recovery_identity() {
    let (source, records) = test_support::pool_settings_evidence(test_support::PoolSettingsCase::Exact);
    let fixture = Fixture::new(source.clone(), records.clone()).await;
    let opened = fixture.recovery.object_reads.load(Ordering::SeqCst);
    let column = fixture
        .catalog
        .get(TelemetryStream::SettingObservations)
        .unwrap()
        .columns()
        .iter()
        .position(|field| field.name == "ConfiguredValue")
        .unwrap();
    let mut rows = fixture.rows.clone();
    rows[0][column] = json!("1");
    fixture.replies(vec![rows]);
    assert!(fixture.reader().read_capture(&fixture.range, FAMILY, EvidenceReadBudget::default(), None).await.is_err());
    assert_eq!(fixture.recovery.object_reads.load(Ordering::SeqCst), opened);
}
