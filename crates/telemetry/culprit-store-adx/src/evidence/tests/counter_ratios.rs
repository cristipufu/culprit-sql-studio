//! Both destinations use the common counter model, projection, publication and original receipt validator.
use super::*;
use std::sync::atomic::Ordering;
use test_support::CounterRatioCase;

async fn collected(case: CounterRatioCase) -> (Fixture, CollectionDescription, Vec<TelemetryRecord>) {
    let (source, records) = test_support::counter_ratio_evidence(case);
    let catalog = Arc::new(SchemaCatalog::new().unwrap());
    let recovery =
        test_support::collected_resource_recovery("adx:test", catalog.digest(), source.clone(), records.clone()).await;
    (Fixture::from_recovery(catalog, Arc::new(recovery)).await, source, records)
}

#[tokio::test]
async fn real_sealed_restarted_published_counter_sets_keep_exact_inputs_and_qualified_missing_values() {
    for case in CounterRatioCase::ALL {
        let (fixture, source, records) = collected(case).await;
        let opened = fixture.recovery.object_reads.load(Ordering::SeqCst);
        fixture.replies(if fixture.rows.is_empty() { vec![] } else { vec![fixture.rows.clone()] });
        let capture = fixture
            .reader()
            .read_capture(&fixture.range, &[EvidenceFamily::OperationalSamples], EvidenceReadBudget::default(), None)
            .await
            .unwrap();
        assert!(capture.is_read_complete(), "{case:?}: {:?}", capture.gaps());
        assert_eq!(capture.source(), &source);
        assert_eq!(capture.captured_rows(TelemetryStream::OperationalSamples), Some(records.len() as u32));
        for (actual, native) in capture.records().iter().zip(&records) {
            assert_eq!(actual.row(), &ExportedRecord::new(&source, capture.capture(), native.clone()).unwrap());
            let TelemetryRecord::Operational(OperationalObservation { fact: OperationalFact::CounterSet(set), .. }) =
                &actual.row().record
            else {
                panic!()
            };
            assert!(set.is_valid());
            if case == CounterRatioCase::Exact {
                assert_eq!(set.counters[0].value.integer(), Some(9_007_199_254_740_993));
                assert_eq!(set.ratios[0].percent(set).as_deref(), Some("99.99"));
            }
            if case == CounterRatioCase::MissingBoth {
                assert_eq!((&set.ratios[0].numerator, &set.ratios[0].base), (&None, &None));
                assert_eq!(set.counters.len(), 1);
            }
            if case == CounterRatioCase::Negative {
                assert_eq!(set.ratio_gaps[0].raw_value, "-1");
            }
            if !matches!(
                case,
                CounterRatioCase::Exact
                    | CounterRatioCase::ZeroNumerator
                    | CounterRatioCase::MultipleInstances
                    | CounterRatioCase::PartialSource
            ) {
                assert!(set.ratios.iter().all(|ratio| ratio.percent(set).is_none()));
            }
        }
        assert_eq!(capture.metrics().recovery_objects, 0);
        assert_eq!(fixture.recovery.object_reads.load(Ordering::SeqCst), opened);
    }
}

#[tokio::test]
async fn changed_pair_type_scope_clock_value_identity_and_null_projection_cannot_reuse_original_receipts() {
    let (fixture, _, _) = collected(CounterRatioCase::Exact).await;
    let schema = fixture.catalog.get(TelemetryStream::OperationalSamples).unwrap();
    let column = schema.columns().iter().position(|column| column.name == "Details").unwrap();
    for (pointer, value) in [
        ("/ratios/0/base", Value::Null),
        ("/ratios/0/support", json!("unknown")),
        ("/ratios/0/observed_at", json!("2020-01-01T12:00:00.6Z")),
        ("/counters/0/value/value", json!("9007199254740992")),
        ("/counters/0/native_type", json!("65792")),
        ("/counters/1/native_instance", json!("foreign")),
        ("/identity/id", json!("foreign-source")),
        ("/scope", json!("database")),
    ] {
        let mut rows = fixture.rows.clone();
        let mut details: Value = serde_json::from_str(rows[0][column].as_str().unwrap()).unwrap();
        *details.pointer_mut(pointer).unwrap() = value;
        rows[0][column] = json!(serde_json::to_string(&details).unwrap());
        fixture.replies(vec![rows]);
        let opened = fixture.recovery.object_reads.load(Ordering::SeqCst);
        assert!(
            fixture
                .reader()
                .read_capture(
                    &fixture.range,
                    &[EvidenceFamily::OperationalSamples],
                    EvidenceReadBudget::default(),
                    Some(fixture.recovery.as_ref())
                )
                .await
                .is_err(),
            "{pointer}"
        );
        assert_eq!(fixture.recovery.object_reads.load(Ordering::SeqCst), opened);
    }
    let mut rows = fixture.rows.clone();
    rows[0][column] = Value::Null;
    fixture.replies(vec![rows]);
    assert!(
        fixture
            .reader()
            .read_capture(&fixture.range, &[EvidenceFamily::OperationalSamples], EvidenceReadBudget::default(), None)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn omitted_counter_body_is_partial_even_with_recovery_enabled_and_keeps_original_source_count() {
    let (fixture, source, _) = collected(CounterRatioCase::Exact).await;
    // Publication and membership are followed by exactly one bounded ordinal-page lookup. An absent
    // warehouse body is an empty successful page, not an omitted mock response or a retry.
    fixture.replies(vec![vec![]]);
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
    assert_eq!(capture.captured_rows(TelemetryStream::OperationalSamples), Some(1));
    assert!(capture.records().is_empty());
    assert!(capture.gaps().iter().any(|gap| gap.reason == EvidenceGapReason::MissingRows));
    assert_eq!(capture.metrics().queries, 3, "one publication, one membership audit and one ordinal-page lookup");
    assert_eq!(capture.metrics().pages, 1);
    assert_eq!(fixture.query.requests.lock().unwrap().len(), 3);
    assert!(fixture.query.replies.lock().unwrap().is_empty());
    assert_eq!(capture.metrics().recovery_objects, 0);
    assert_eq!(fixture.recovery.object_reads.load(Ordering::SeqCst), opened);
}
