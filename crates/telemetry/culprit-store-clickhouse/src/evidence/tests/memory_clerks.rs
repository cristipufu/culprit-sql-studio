//! Native clerk sets reuse the existing physical codec, publication proof and exact original receipts.
use super::*;
use std::sync::atomic::Ordering;
use test_support::MemoryClerkCase;

async fn collected(case: MemoryClerkCase) -> (Fixture, CollectionDescription, Vec<TelemetryRecord>) {
    let (source, records) = test_support::memory_clerk_evidence(case);
    let catalog = Arc::new(SchemaCatalog::new().unwrap());
    let recovery =
        test_support::collected_resource_recovery("clickhouse:test", catalog.digest(), source.clone(), records.clone())
            .await;
    (Fixture::from_recovery(catalog, Arc::new(recovery), &[TelemetryStream::OperationalSamples]).await, source, records)
}

#[tokio::test]
async fn original_clerk_groups_keep_exact_independent_allocations_scope_coverage_and_unrelated_rows() {
    for case in MemoryClerkCase::ALL {
        let (fixture, source, records) = collected(case).await;
        let opened = fixture.recovery.object_reads.load(Ordering::SeqCst);
        fixture.replies(vec![fixture.rows.clone()]);
        let capture = fixture
            .reader
            .read_capture(&fixture.range, &[EvidenceFamily::OperationalSamples], EvidenceReadBudget::default(), None)
            .await
            .unwrap();
        test_support::assert_operational_capture(&capture, &source, &records);
        assert!(capture.is_read_complete());
        if case == MemoryClerkCase::SharedInstance {
            let CollectionTarget::Shared(instance) = &capture.source().target else {
                panic!("native shared clerk source")
            };
            assert_eq!(instance.kind, SharedResourceKind::Instance);
            assert_eq!(capture.source().partition, source.partition);
            assert!(capture.source().target.database().is_none());
        }
        let original = source.observation.snapshot().unwrap();
        assert_eq!(capture.source().observation.snapshot().unwrap().sources, original.sources);
        assert_eq!(capture.captured_rows(TelemetryStream::OperationalSamples), Some(records.len() as u32));
        for (actual, native) in capture.records().iter().zip(&records) {
            assert_eq!(actual.row(), &ExportedRecord::new(&source, capture.capture(), native.clone()).unwrap());
        }
        let clerks = capture
            .records()
            .iter()
            .filter_map(|record| {
                let TelemetryRecord::Operational(row) = &record.row().record else { panic!() };
                if row.source != culprit_core::source_id::MEMORY_CLERKS {
                    return None;
                }
                let OperationalFact::CounterSet(set) = &row.fact else { panic!() };
                Some(set)
            })
            .collect::<Vec<_>>();
        assert_eq!(clerks.len(), original.sources[0].records as usize);
        if matches!(case, MemoryClerkCase::Exact | MemoryClerkCase::SharedInstance) {
            assert_eq!(clerks.len(), 2);
            assert_ne!(clerks[0].identity, clerks[1].identity);
            for set in clerks {
                assert_eq!(set.scope, culprit_core::MeasurementScope::Server);
                assert_eq!(
                    set.counters.iter().find(|counter| counter.key == "pages_kb").unwrap().value.integer(),
                    Some(9_007_199_254_740_993)
                );
                assert!(set.counters.iter().all(|counter| counter.nature == CounterNature::Gauge
                    && counter.reset == culprit_core::ResetMarker::Unknown));
                assert!(!set.counters.iter().any(|counter| counter.key == "shared_memory_reserved_kb"));
                assert_eq!(
                    set.counters
                        .iter()
                        .find(|counter| counter.key == "shared_memory_committed_kb")
                        .unwrap()
                        .value
                        .integer(),
                    Some(0)
                );
            }
        }
        assert_eq!(capture.metrics().recovery_objects, 0);
        assert_eq!(fixture.recovery.object_reads.load(Ordering::SeqCst), opened);
    }
}

#[tokio::test]
async fn missing_clerk_bodies_keep_original_population_and_unrelated_source_without_recovery_open() {
    let (fixture, source, _) = collected(MemoryClerkCase::Exact).await;
    let schema = fixture.catalog.get(TelemetryStream::OperationalSamples).unwrap();
    let native_source = schema.columns().iter().position(|column| column.name == "NativeSource").unwrap();
    let kept = fixture
        .rows
        .iter()
        .filter(|row| row[native_source] != culprit_core::source_id::MEMORY_CLERKS)
        .cloned()
        .collect();
    fixture.replies(vec![kept]);
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
    assert!(capture.gaps().iter().any(|gap| gap.reason == EvidenceGapReason::MissingRows));
    assert_eq!(capture.source(), &source);
    assert_eq!(capture.captured_rows(TelemetryStream::OperationalSamples), Some(3));
    assert_eq!(capture.records().len(), 1);
    let TelemetryRecord::Operational(row) = &capture.records()[0].row().record else { panic!() };
    assert_eq!(row.source, "independent-native-counters");
    assert_eq!(source.observation.snapshot().unwrap().sources[0].records, 2);
    assert_eq!(capture.metrics().recovery_objects, 0);
    assert_eq!(fixture.recovery.object_reads.load(Ordering::SeqCst), opened);
}

#[tokio::test]
async fn changed_allocation_node_scope_reset_or_missing_details_cannot_borrow_original_receipts() {
    for (pointer, value) in [
        ("/counters/1/value/value", json!("9007199254740992")),
        ("/identity/qualifiers/memory_node_id", json!("2")),
        ("/scope", json!("database")),
        ("/counters/1/reset", json!({"kind":"generation","value":"foreign-reset"})),
        ("/properties/name", json!("unrelated-clerk-name")),
    ] {
        let (fixture, _, _) = collected(MemoryClerkCase::Exact).await;
        let schema = fixture.catalog.get(TelemetryStream::OperationalSamples).unwrap();
        let column = schema.columns().iter().position(|column| column.name == "Details").unwrap();
        let mut rows = fixture.rows.clone();
        let mut details: Value = serde_json::from_str(rows[0][column].as_str().unwrap()).unwrap();
        *details.pointer_mut(pointer).unwrap() = value;
        rows[0][column] = json!(serde_json::to_string(&details).unwrap());
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
    let (fixture, _, _) = collected(MemoryClerkCase::Exact).await;
    let column = fixture
        .catalog
        .get(TelemetryStream::OperationalSamples)
        .unwrap()
        .columns()
        .iter()
        .position(|column| column.name == "Details")
        .unwrap();
    let mut rows = fixture.rows.clone();
    rows[0][column] = Value::Null;
    fixture.replies(vec![rows]);
    let opened = fixture.recovery.object_reads.load(Ordering::SeqCst);
    assert!(
        fixture
            .reader
            .read_capture(&fixture.range, &[EvidenceFamily::OperationalSamples], EvidenceReadBudget::default(), None)
            .await
            .is_err()
    );
    assert_eq!(fixture.recovery.object_reads.load(Ordering::SeqCst), opened);
}
