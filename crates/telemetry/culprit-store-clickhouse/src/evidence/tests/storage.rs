use super::*;
use std::sync::atomic::Ordering;
use test_support::StorageCase;
async fn collected(case: StorageCase) -> (Fixture, CollectionDescription, Vec<TelemetryRecord>) {
    let (source, rows) = test_support::storage_evidence(case);
    let catalog = Arc::new(SchemaCatalog::new().unwrap());
    let recovery =
        test_support::collected_resource_recovery("clickhouse:test", catalog.digest(), source.clone(), rows.clone())
            .await;
    (Fixture::from_recovery(catalog, Arc::new(recovery), &[TelemetryStream::OperationalSamples]).await, source, rows)
}
#[tokio::test]
async fn collected_sealed_restarted_published_storage_keeps_all_original_fields_and_exact_integer_strings() {
    for case in StorageCase::ALL {
        let (fixture, source, rows) = collected(case).await;
        fixture.replies(if fixture.rows.is_empty() { vec![] } else { vec![fixture.rows.clone()] });
        let opened = fixture.recovery.object_reads.load(Ordering::SeqCst);
        let capture = fixture
            .reader
            .read_capture(&fixture.range, &[EvidenceFamily::OperationalSamples], EvidenceReadBudget::default(), None)
            .await
            .unwrap();
        test_support::assert_operational_capture(&capture, &source, &rows);
        assert!(capture.is_read_complete());
        for (actual, native) in capture.records().iter().zip(&rows) {
            assert_eq!(actual.row(), &ExportedRecord::new(&source, capture.capture(), native.clone()).unwrap());
        }
        assert_eq!(capture.captured_rows(TelemetryStream::OperationalSamples), Some(rows.len() as u32));
        assert_eq!(fixture.recovery.object_reads.load(Ordering::SeqCst), opened);
    }
}
#[tokio::test]
async fn missing_storage_bodies_keep_original_native_population_and_never_hydrate_from_recovery() {
    let (fixture, source, _) = collected(StorageCase::Exact).await;
    fixture.replies(vec![vec![]]);
    let opened = fixture.recovery.object_reads.load(Ordering::SeqCst);
    let capture = fixture
        .reader
        .read_capture(
            &fixture.range,
            &[EvidenceFamily::OperationalSamples],
            EvidenceReadBudget::default(),
            Some(fixture.recovery.as_ref()),
        )
        .await
        .unwrap();
    assert!(!capture.is_read_complete());
    assert!(capture.records().is_empty());
    assert_eq!(capture.captured_rows(TelemetryStream::OperationalSamples), Some(2));
    assert_eq!(capture.source(), &source);
    assert!(capture.gaps().iter().any(|gap| gap.reason == EvidenceGapReason::MissingRows));
    assert_eq!(fixture.recovery.object_reads.load(Ordering::SeqCst), opened);
}
#[tokio::test]
async fn changed_integer_population_file_state_or_unit_cannot_borrow_original_receipts() {
    for (row_index, pointer, value) in [
        (0, "/total_bytes", json!("9007199254740992")),
        (0, "/population", json!("database-transaction-log")),
        (1, "/native_state", json!("6")),
        (1, "/page_size_bytes", json!(4096)),
        (1, "/used_pages", Value::Null),
    ] {
        let (fixture, _, _) = collected(StorageCase::Exact).await;
        let column = fixture
            .catalog
            .get(TelemetryStream::OperationalSamples)
            .unwrap()
            .columns()
            .iter()
            .position(|column| column.name == "Details")
            .unwrap();
        let mut rows = fixture.rows.clone();
        let mut detail: Value = serde_json::from_str(rows[row_index][column].as_str().unwrap()).unwrap();
        *detail.pointer_mut(pointer).unwrap() = value;
        rows[row_index][column] = json!(serde_json::to_string(&detail).unwrap());
        fixture.replies(vec![rows]);
        let opened = fixture.recovery.object_reads.load(Ordering::SeqCst);
        assert!(
            fixture
                .reader
                .read_capture(
                    &fixture.range,
                    &[EvidenceFamily::OperationalSamples],
                    EvidenceReadBudget::default(),
                    Some(fixture.recovery.as_ref())
                )
                .await
                .is_err()
        );
        assert_eq!(fixture.recovery.object_reads.load(Ordering::SeqCst), opened);
    }
}
