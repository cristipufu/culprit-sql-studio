//! Both destinations replay the same original native backup corpus without recovery or source work.
use super::*;
use test_support::BackupHistoryCase;
const FAMILY: &[EvidenceFamily] = &[EvidenceFamily::OperationalSamples];
#[tokio::test]
async fn backup_metadata_preserves_native_profiles_exact_numbers_and_unavailable_empty_distinctions() {
    for case in BackupHistoryCase::ALL {
        let (source, records) = test_support::backup_history_evidence(case);
        let fixture = Fixture::new(source.clone(), records.clone()).await;
        fixture.replies(fixture.rows.iter().map(|row| vec![row.clone()]).collect());
        let capture = fixture
            .reader
            .read_capture(&fixture.range, FAMILY, EvidenceReadBudget { page_rows: 1, ..Default::default() }, None)
            .await
            .unwrap();
        test_support::assert_backup_history_capture(&capture, &source, &records);
        assert_eq!(capture.metrics().pages as usize, records.len(), "{case:?}");
        assert!(matches!(*fixture.recovery.fault.lock().unwrap(), Fault::Stall));
    }
}
#[tokio::test]
async fn altered_native_backup_scope_or_exact_lsn_cannot_reuse_an_original_receipt() {
    for (pointer, replacement) in [
        ("/scope/database_id", json!(8)),
        ("/record/data/lsns/first", json!("9999999999999999999999998")),
        ("/context/observed_until", json!("2020-01-01T00:00:02Z")),
    ] {
        let (source, records) = test_support::backup_history_evidence(BackupHistoryCase::MsdbNative);
        let fixture = Fixture::new(source, records).await;
        let schema = fixture.catalog.get(TelemetryStream::OperationalSamples).unwrap();
        let details_column = schema.columns().iter().position(|column| column.name == "Details").unwrap();
        let mut rows = fixture.rows.clone();
        let mut details: Value = serde_json::from_str(rows[0][details_column].as_str().unwrap()).unwrap();
        *details.pointer_mut(pointer).unwrap() = replacement;
        rows[0][details_column] = json!(serde_json::to_string(&details).unwrap());
        fixture.replies(vec![rows]);
        let error =
            fixture.reader.read_capture(&fixture.range, FAMILY, EvidenceReadBudget::default(), None).await.unwrap_err();
        assert_eq!(error.detail, Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::Integrity)), "{pointer}");
    }
}
