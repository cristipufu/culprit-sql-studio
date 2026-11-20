//! Native counter sets reuse the existing physical codec, publication proof and exact original receipts.
use super::*;
use std::sync::atomic::Ordering;
use test_support::PerformanceCounterCase;

async fn collected(case: PerformanceCounterCase) -> (Fixture, CollectionDescription, Vec<TelemetryRecord>) {
    let (source, records) = test_support::performance_counter_evidence(case);
    let catalog = Arc::new(SchemaCatalog::new().unwrap());
    let recovery =
        test_support::collected_resource_recovery("adx:test", catalog.digest(), source.clone(), records.clone()).await;
    (Fixture::from_recovery(catalog, Arc::new(recovery)).await, source, records)
}

#[tokio::test]
async fn exact_shared_native_counter_inputs_keep_scope_instances_clocks_and_source_counts_without_hydration() {
    for case in PerformanceCounterCase::ALL {
        let (fixture, source, records) = collected(case).await;
        fixture.replies(vec![fixture.rows.clone()]);
        let opened = fixture.recovery.object_reads.load(Ordering::SeqCst);
        let capture = fixture
            .reader()
            .read_capture(&fixture.range, &[EvidenceFamily::OperationalSamples], EvidenceReadBudget::default(), None)
            .await
            .unwrap();
        test_support::assert_operational_capture(&capture, &source, &records);
        assert_eq!(capture.source(), &source);
        assert!(capture.is_read_complete());
        assert_eq!(capture.captured_rows(TelemetryStream::OperationalSamples), Some(records.len() as u32));
        for (actual, original) in capture.records().iter().zip(&records) {
            assert_eq!(actual.row(), &ExportedRecord::new(&source, capture.capture(), original.clone()).unwrap());
        }
        if case != PerformanceCounterCase::Denied {
            let TelemetryRecord::Operational(OperationalObservation { fact: OperationalFact::CounterSet(set), .. }) =
                &capture.records()[0].row().record
            else {
                panic!()
            };
            assert_eq!(set.scope, culprit_core::MeasurementScope::Server);
            let field = &set.counters[0];
            assert_eq!(
                field.value.integer(),
                Some(if case == PerformanceCounterCase::MeasuredZero { 0 } else { 9_007_199_254_740_993 })
            );
            assert_eq!(field.reset, culprit_core::ResetMarker::Unknown);
            assert_eq!(set.ratios.len(), 2);
            assert_ne!(set.ratios[0].numerator, set.ratios[1].numerator);
            assert_eq!(set.ratios[0].observed_at.is_none(), case == PerformanceCounterCase::ClockUnavailable);
            if case == PerformanceCounterCase::UnknownNativeType {
                assert_eq!(field.unit, CounterUnit::Unknown);
            }
            if case == PerformanceCounterCase::MissingFields {
                assert!(set.ratios.iter().any(|pair| pair.base.is_none()));
            }
        }
        assert_eq!(capture.metrics().recovery_objects, 0);
        assert_eq!(fixture.recovery.object_reads.load(Ordering::SeqCst), opened);
    }
}

#[tokio::test]
async fn missing_counter_set_body_retains_original_native_coverage_and_an_unrelated_source() {
    let (fixture, source, _) = collected(PerformanceCounterCase::Exact).await;
    let schema = fixture.catalog.get(TelemetryStream::OperationalSamples).unwrap();
    let column = schema.columns().iter().position(|column| column.name == "NativeSource").unwrap();
    let rows = fixture
        .rows
        .iter()
        .filter(|row| row[column] != culprit_core::source_id::PERFORMANCE_COUNTERS)
        .cloned()
        .collect();
    fixture.replies(vec![rows]);
    let opened = fixture.recovery.object_reads.load(Ordering::SeqCst);
    let capture = fixture
        .reader()
        .read_capture(
            &fixture.range,
            &[EvidenceFamily::OperationalSamples],
            EvidenceReadBudget::default(),
            Some(fixture.recovery.as_ref()),
        )
        .await
        .unwrap();
    assert!(!capture.is_read_complete());
    assert_eq!(capture.source(), &source);
    assert_eq!(capture.records().len(), 1);
    assert_eq!(capture.captured_rows(TelemetryStream::OperationalSamples), Some(2));
    assert!(capture.gaps().iter().any(|gap| gap.reason == EvidenceGapReason::MissingRows));
    assert_eq!(source.observation.snapshot().unwrap().sources[0].records, 1);
    assert_eq!(fixture.recovery.object_reads.load(Ordering::SeqCst), opened);
}

#[tokio::test]
async fn changed_value_native_type_instance_or_read_clock_cannot_borrow_original_counter_receipts() {
    for (pointer, value) in [
        ("/counters/0/value/value", json!("9007199254740992")),
        ("/counters/0/native_type", json!("42")),
        ("/counters/5/native_instance", json!("Object Plans")),
        ("/properties/counter_observed_at", json!("2020-01-01T12:00:00.600000000Z")),
    ] {
        let (fixture, _, _) = collected(PerformanceCounterCase::Exact).await;
        let schema = fixture.catalog.get(TelemetryStream::OperationalSamples).unwrap();
        let column = schema.columns().iter().position(|column| column.name == "Details").unwrap();
        let mut rows = fixture.rows.clone();
        let mut details: Value = serde_json::from_str(rows[0][column].as_str().unwrap()).unwrap();
        *details.pointer_mut(pointer).unwrap() = value;
        rows[0][column] = json!(serde_json::to_string(&details).unwrap());
        fixture.replies(vec![rows]);
        let opened = fixture.recovery.object_reads.load(Ordering::SeqCst);
        let error = fixture
            .reader()
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

#[tokio::test]
async fn selected_database_counters_restore_only_original_native_database_proof_without_publishing_or_hydrating_it() {
    use test_support::{DatabasePerformanceCounterCase as Case, database_performance_counter_evidence};
    for case in Case::ALL {
        let (source, records) = database_performance_counter_evidence(case);
        let catalog = Arc::new(SchemaCatalog::new().unwrap());
        let recovery =
            test_support::collected_resource_recovery("adx:test", catalog.digest(), source.clone(), records.clone())
                .await;
        let fixture = Fixture::from_recovery(catalog, Arc::new(recovery)).await;
        let public = serde_json::to_string(&fixture.rows).unwrap();
        for private in [
            "private-database-guid-canary",
            "private-fork-guid-canary",
            "private-file-guid-canary",
            "private-process-canary",
            "private-machine-canary",
            "private-member-canary",
        ] {
            assert!(!public.contains(private), "{private}");
        }
        fixture.replies(vec![fixture.rows.clone()]);
        let opened = fixture.recovery.object_reads.load(Ordering::SeqCst);
        let capture = fixture
            .reader()
            .read_capture(
                &fixture.range,
                &[EvidenceFamily::OperationalSamples],
                EvidenceReadBudget { page_rows: 1, ..EvidenceReadBudget::default() },
                None,
            )
            .await
            .unwrap();
        test_support::assert_operational_capture(&capture, &source, &records);
        assert!(capture.is_read_complete());
        assert_eq!(capture.source(), &source);
        for (actual, original) in capture.records().iter().zip(&records) {
            assert_eq!(actual.row(), &ExportedRecord::new(&source, capture.capture(), original.clone()).unwrap());
        }
        if case != Case::Denied {
            let TelemetryRecord::Operational(OperationalObservation { fact: OperationalFact::CounterSet(set), .. }) =
                &capture.records()[0].row().record
            else {
                panic!()
            };
            assert_eq!(set.identity.qualifiers["database_guid"], "private-database-guid-canary");
            assert_eq!(set.scope, culprit_core::MeasurementScope::Database);
            assert_eq!(set.counters[0].value.integer(), Some(9_007_199_254_740_993));
        }
        assert_eq!(capture.metrics().recovery_objects, 0);
        assert_eq!(fixture.recovery.object_reads.load(Ordering::SeqCst), opened);
    }
}
