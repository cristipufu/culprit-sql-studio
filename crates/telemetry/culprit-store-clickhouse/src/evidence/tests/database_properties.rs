//! The generic Operational codec retains original property points; no destination-specific interpretation.
use super::*;
use std::sync::atomic::Ordering;
use test_support::DatabasePropertiesCase;
async fn collected(case: DatabasePropertiesCase) -> (Fixture, CollectionDescription, Vec<TelemetryRecord>) {
    let (source, records) = test_support::database_properties_evidence(case);
    let catalog = Arc::new(SchemaCatalog::new().unwrap());
    let recovery =
        test_support::collected_resource_recovery("clickhouse:test", catalog.digest(), source.clone(), records.clone())
            .await;
    (Fixture::from_recovery(catalog, Arc::new(recovery), &[TelemetryStream::OperationalSamples]).await, source, records)
}
#[tokio::test]
async fn genuine_collected_sealed_published_property_points_retain_all_original_fields_and_source_outcomes() {
    for case in DatabasePropertiesCase::ALL {
        let (fixture, source, records) = collected(case).await;
        let opened = fixture.recovery.object_reads.load(Ordering::SeqCst);
        fixture.replies(if fixture.rows.is_empty() { vec![] } else { vec![fixture.rows.clone()] });
        let capture = fixture
            .reader
            .read_capture(&fixture.range, &[EvidenceFamily::OperationalSamples], EvidenceReadBudget::default(), None)
            .await
            .unwrap();
        test_support::assert_operational_capture(&capture, &source, &records);
        assert!(capture.is_read_complete());
        assert_eq!(capture.captured_rows(TelemetryStream::OperationalSamples), Some(records.len() as u32));
        assert_eq!(capture.source().partition, SourcePartition::Target);
        for (actual, native) in capture.records().iter().zip(&records) {
            assert_eq!(actual.row(), &ExportedRecord::new(&source, capture.capture(), native.clone()).unwrap());
            let TelemetryRecord::Operational(OperationalObservation {
                fact: OperationalFact::DatabaseProperties(point),
                ..
            }) = &actual.row().record
            else {
                panic!()
            };
            assert_eq!(point.native_database_id, "5");
            assert_eq!(point.database, "sample");
            assert_eq!(point.catalog_read_only, Some(false));
            let expected = match case {
                DatabasePropertiesCase::UnknownValues => None,
                DatabasePropertiesCase::NoRecordedCheckDb => Some("1900-01-01T00:00:00.000"),
                _ => Some("2019-12-28T23:10:00.003"),
            };
            assert_eq!(point.last_good_checkdb_without_offset.as_deref(), expected);
            assert_eq!(point.observed_at, "2020-01-01T12:00:00.0000001Z");
        }
        assert_eq!(fixture.recovery.object_reads.load(Ordering::SeqCst), opened);
        assert_eq!(capture.metrics().recovery_objects, 0);
    }
}
#[tokio::test]
async fn missing_property_body_preserves_original_native_count_without_recovery_hydration() {
    let (fixture, source, _) = collected(DatabasePropertiesCase::Exact).await;
    let opened = fixture.recovery.object_reads.load(Ordering::SeqCst);
    fixture.replies(vec![vec![]]);
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
    assert!(capture.gaps().iter().any(|gap| gap.reason == EvidenceGapReason::MissingRows));
    assert_eq!(capture.source(), &source);
    assert_eq!(capture.captured_rows(TelemetryStream::OperationalSamples), Some(1));
    assert!(capture.records().is_empty());
    assert_eq!(fixture.recovery.object_reads.load(Ordering::SeqCst), opened);
}
#[tokio::test]
async fn native_code_clock_database_flag_and_null_projection_tampering_never_borrow_original_receipts() {
    for (pointer, replacement) in [
        ("/catalog_state/code", json!(1)),
        ("/observed_at", json!("2020-01-01T12:00:00.0000002Z")),
        ("/database", json!("foreign")),
        ("/catalog_read_only", json!(true)),
        ("/query_store_enabled", Value::Null),
        ("/native_database_id", json!("6")),
        ("/last_good_checkdb_without_offset", Value::Null),
        ("/last_good_checkdb_without_offset", json!("2019-12-28T23:10:00.007")),
    ] {
        let (fixture, _, _) = collected(DatabasePropertiesCase::Exact).await;
        let column = fixture
            .catalog
            .get(TelemetryStream::OperationalSamples)
            .unwrap()
            .columns()
            .iter()
            .position(|column| column.name == "Details")
            .unwrap();
        let mut rows = fixture.rows.clone();
        let mut detail: Value = serde_json::from_str(rows[0][column].as_str().unwrap()).unwrap();
        *detail.pointer_mut(pointer).unwrap() = replacement;
        rows[0][column] = json!(serde_json::to_string(&detail).unwrap());
        fixture.replies(vec![rows]);
        let opened = fixture.recovery.object_reads.load(Ordering::SeqCst);
        let error = fixture
            .reader
            .read_capture(
                &fixture.range,
                &[EvidenceFamily::OperationalSamples],
                EvidenceReadBudget::default(),
                Some(fixture.recovery.as_ref()),
            )
            .await
            .unwrap_err();
        assert_eq!(error.detail, Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::Integrity)), "{pointer}");
        assert_eq!(fixture.recovery.object_reads.load(Ordering::SeqCst), opened);
    }
}
