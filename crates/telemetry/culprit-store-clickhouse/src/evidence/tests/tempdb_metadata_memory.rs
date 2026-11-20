//! Destination codecs retain original metadata-memory evidence without reinterpreting native ownership.
use super::*;
use std::sync::atomic::Ordering;
use test_support::TempdbMetadataMemoryCase;

async fn collected(case: TempdbMetadataMemoryCase) -> (Fixture, CollectionDescription, Vec<TelemetryRecord>) {
    let (source, records) = test_support::tempdb_metadata_memory_evidence(case);
    let catalog = Arc::new(SchemaCatalog::new().unwrap());
    let recovery =
        test_support::collected_resource_recovery("clickhouse:test", catalog.digest(), source.clone(), records.clone())
            .await;
    (Fixture::from_recovery(catalog, Arc::new(recovery), &[TelemetryStream::OperationalSamples]).await, source, records)
}

#[tokio::test]
async fn native_feature_and_consumer_points_preserve_exact_scope_bytes_and_independent_partial_coverage() {
    for case in TempdbMetadataMemoryCase::ALL {
        let (fixture, source, records) = collected(case).await;
        let opened = fixture.recovery.object_reads.load(Ordering::SeqCst);
        fixture.replies(vec![fixture.rows.clone()]);
        let capture = fixture
            .reader
            .read_capture(&fixture.range, &[EvidenceFamily::OperationalSamples], EvidenceReadBudget::default(), None)
            .await
            .unwrap();
        test_support::assert_tempdb_metadata_memory_capture(&capture, &source, &records, case);
        assert_eq!(capture.metrics().recovery_objects, 0);
        assert_eq!(fixture.recovery.object_reads.load(Ordering::SeqCst), opened);
    }
}

#[tokio::test]
async fn missing_feature_body_retains_original_population_and_present_consumer_without_implicit_recovery() {
    let (fixture, source, _) = collected(TempdbMetadataMemoryCase::Reserved).await;
    fixture.replies(vec![fixture.rows[1..].to_vec()]);
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
    assert_eq!(capture.source(), &source);
    assert!(!capture.is_read_complete());
    assert!(capture.gaps().iter().any(|gap| gap.reason == EvidenceGapReason::MissingRows));
    assert_eq!(capture.captured_rows(TelemetryStream::OperationalSamples), Some(3));
    assert_eq!(capture.records().len(), 2);
    assert_eq!(source.observation.snapshot().unwrap().sources[0].records, 2);
    let TelemetryRecord::Operational(row) = &capture.records()[0].row().record else { panic!("native consumer") };
    let OperationalFact::CounterSet(set) = &row.fact else { panic!("native counters") };
    let context = NativeReadContext::from_snapshot(&source).unwrap();
    assert_eq!(
        TempdbMetadataConsumer::from_original(set, &context).unwrap().allocated_bytes(),
        Some("9007199254740993")
    );
    assert_eq!(capture.metrics().recovery_objects, 0);
    assert_eq!(fixture.recovery.object_reads.load(Ordering::SeqCst), opened);
}

#[tokio::test]
async fn rewritten_feature_bytes_scope_or_native_clock_cannot_borrow_the_original_publication_receipt() {
    for (index, pointer, value) in [
        (0, "/properties/metadata_enabled", json!("0")),
        (1, "/counters/0/value/value", json!("9007199254740992")),
        (1, "/scope", json!("database")),
        (1, "/properties/observed_at", json!("2020-01-01T12:00:00.6Z")),
    ] {
        let (fixture, _, _) = collected(TempdbMetadataMemoryCase::Reserved).await;
        let schema = fixture.catalog.get(TelemetryStream::OperationalSamples).unwrap();
        let column = schema.columns().iter().position(|column| column.name == "Details").unwrap();
        let mut rows = fixture.rows.clone();
        let mut details: Value = serde_json::from_str(rows[index][column].as_str().unwrap()).unwrap();
        *details.pointer_mut(pointer).unwrap() = value;
        rows[index][column] = json!(serde_json::to_string(&details).unwrap());
        fixture.replies(vec![rows]);
        let opened = fixture.recovery.object_reads.load(Ordering::SeqCst);
        let error = fixture
            .reader
            .read_capture(&fixture.range, &[EvidenceFamily::OperationalSamples], EvidenceReadBudget::default(), None)
            .await
            .unwrap_err();
        assert_eq!(error.detail, Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::Integrity)), "{pointer}");
        assert_eq!(fixture.recovery.object_reads.load(Ordering::SeqCst), opened);
    }
}
