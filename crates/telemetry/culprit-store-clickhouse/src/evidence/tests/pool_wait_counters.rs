use super::*;
use std::sync::atomic::Ordering;
const FAMILY: &[EvidenceFamily] = &[EvidenceFamily::WaitCounterSamples];
#[tokio::test]
async fn pool_wait_current_authority_exact_integer_reset_and_partial_coverage_roundtrip_without_recovery() {
    for case in test_support::PoolWaitCounterCase::ALL {
        let (source, records) = test_support::pool_wait_counter_evidence(case);
        let fixture =
            Fixture::new_for_stream(source.clone(), records.clone(), TelemetryStream::WaitCounterSamples).await;
        let opened = fixture.recovery.object_reads.load(Ordering::SeqCst);
        fixture.replies(fixture.rows.iter().map(|row| vec![row.clone()]).collect());
        let capture = fixture
            .reader
            .read_capture(&fixture.range, FAMILY, EvidenceReadBudget { page_rows: 1, ..Default::default() }, None)
            .await
            .unwrap();
        assert_eq!(capture.source(), &source);
        assert!(capture.is_read_complete());
        assert_eq!(capture.records().iter().map(|row| row.row().record.clone()).collect::<Vec<_>>(), records);
        assert_eq!(
            mssql_pool_current_authority(capture.source()).unwrap(),
            mssql_pool_current_authority(&source).unwrap()
        );
        assert_eq!(capture.metrics().recovery_objects, 0);
        assert_eq!(fixture.recovery.object_reads.load(Ordering::SeqCst), opened);
    }
}
#[tokio::test]
async fn edited_pool_wait_total_cannot_borrow_the_original_native_row_receipt() {
    let (source, records) = test_support::pool_wait_counter_evidence(test_support::PoolWaitCounterCase::GeneralPurpose);
    let fixture = Fixture::new_for_stream(source.clone(), records.clone(), TelemetryStream::WaitCounterSamples).await;
    let column = fixture
        .catalog
        .get(TelemetryStream::WaitCounterSamples)
        .unwrap()
        .columns()
        .iter()
        .position(|field| field.name == "WaitTimeMs")
        .unwrap();
    let mut rows = fixture.rows.clone();
    rows[0][column] = json!("9007199254740994");
    fixture.replies(vec![rows]);
    assert!(fixture.reader.read_capture(&fixture.range, FAMILY, EvidenceReadBudget::default(), None).await.is_err());
}
