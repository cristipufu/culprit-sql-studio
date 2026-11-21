//! Native counter sets reuse the existing physical codec, publication proof and exact original receipts.
use super::*;
use std::sync::atomic::Ordering;
use test_support::PerformanceCounterCase;

async fn collected(case: PerformanceCounterCase) -> (Fixture, CollectionDescription, Vec<TelemetryRecord>) {
    let (source, records) = test_support::performance_counter_evidence(case);
    let fixture = collected_original(source.clone(), records.clone()).await;
    (fixture, source, records)
}

async fn collected_original(source: CollectionDescription, records: Vec<TelemetryRecord>) -> Fixture {
    let catalog = Arc::new(SchemaCatalog::new().unwrap());
    let recovery = test_support::collected_resource_recovery("adx:test", catalog.digest(), source, records).await;
    Fixture::from_recovery(catalog, Arc::new(recovery)).await
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

#[tokio::test]
async fn original_governor_sets_roundtrip_native_ids_nullable_context_and_exact_cumulative_values_without_hydration() {
    use test_support::{ResourceGovernorCounterCase as Case, resource_governor_counter_evidence};
    for case in Case::ALL {
        let (source, records) = resource_governor_counter_evidence(case);
        let fixture = collected_original(source.clone(), records.clone()).await;
        fixture.replies(vec![fixture.rows.clone()]);
        let opened = fixture.recovery.object_reads.load(Ordering::SeqCst);
        let capture = fixture
            .reader()
            .read_capture(&fixture.range, &[EvidenceFamily::OperationalSamples], EvidenceReadBudget::default(), None)
            .await
            .unwrap();
        test_support::assert_operational_capture(&capture, &source, &records);
        assert!(capture.is_read_complete());
        for (actual, original) in capture.records().iter().zip(&records) {
            assert_eq!(actual.row(), &ExportedRecord::new(&source, capture.capture(), original.clone()).unwrap());
        }
        let sets = capture
            .records()
            .iter()
            .filter_map(|record| {
                let TelemetryRecord::Operational(OperationalObservation {
                    fact: OperationalFact::CounterSet(sample),
                    ..
                }) = &record.row().record
                else {
                    panic!()
                };
                resource_governor_counter_set(sample).then_some(sample.as_ref())
            })
            .collect::<Vec<_>>();
        assert_eq!(sets.len(), if matches!(case, Case::Denied | Case::NoRows) { 0 } else { 2 });
        for set in sets {
            assert!(resource_governor_fits_source(set, &source));
            let metadata = MssqlResourceGovernorCounterSet::from_sample(set).unwrap().context();
            assert_eq!(metadata.native_pool_id.as_deref(), if case == Case::Unmapped { None } else { Some("2") });
            assert!(set.counters.iter().all(|counter| counter.reset == culprit_core::ResetMarker::Unknown));
            if !matches!(case, Case::Zero | Case::UnknownNativeType) {
                assert!(set.counters.iter().any(|counter| counter.value.integer() == Some(9_007_199_254_740_993)));
            }
        }
        assert_eq!(capture.metrics().recovery_objects, 0);
        assert_eq!(fixture.recovery.object_reads.load(Ordering::SeqCst), opened);
    }
}

#[tokio::test]
async fn original_common_inputs_and_real_buffer_base_roundtrip_without_recovery_or_reinterpretation() {
    use test_support::{CommonPerformanceCounterCase as Case, common_performance_counter_evidence};
    for case in Case::ALL {
        let (source, records) = common_performance_counter_evidence(case);
        let fixture = collected_original(source.clone(), records.clone()).await;
        fixture.replies(vec![fixture.rows.clone()]);
        let opened = fixture.recovery.object_reads.load(Ordering::SeqCst);
        let capture = fixture
            .reader()
            .read_capture(&fixture.range, &[EvidenceFamily::OperationalSamples], EvidenceReadBudget::default(), None)
            .await
            .unwrap();
        test_support::assert_common_performance_counter_capture(&capture, &source, &records, case);
        assert_eq!(capture.metrics().recovery_objects, 0);
        assert_eq!(fixture.recovery.object_reads.load(Ordering::SeqCst), opened);
    }
}

#[tokio::test]
async fn changed_common_base_cannot_reinterpret_an_authenticated_original_fraction() {
    let (source, records) =
        test_support::common_performance_counter_evidence(test_support::CommonPerformanceCounterCase::Observed);
    let fixture = collected_original(source, records).await;
    let schema = fixture.catalog.get(TelemetryStream::OperationalSamples).unwrap();
    let column = schema.columns().iter().position(|column| column.name == "Details").unwrap();
    let mut rows = fixture.rows.clone();
    let mut changed = false;
    for row in &mut rows {
        let mut details: Value = serde_json::from_str(row[column].as_str().unwrap()).unwrap();
        let Some(counters) = details.get_mut("counters").and_then(Value::as_array_mut) else { continue };
        let Some(base) = counters.iter_mut().find(|counter| counter["native_name"] == "Buffer cache hit ratio base")
        else {
            continue;
        };
        base["value"]["value"] = json!("5");
        row[column] = json!(serde_json::to_string(&details).unwrap());
        changed = true;
    }
    assert!(changed, "tamper the actual native base, not an unrelated row");
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
    assert_eq!(error.detail, Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::Integrity)));
    assert_eq!(fixture.recovery.object_reads.load(Ordering::SeqCst), opened);
}
