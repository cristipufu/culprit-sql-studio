use super::*;
use std::sync::atomic::Ordering;
use test_support::InstanceResourceCase;

async fn collected(case: InstanceResourceCase) -> (Fixture, CollectionDescription, Vec<TelemetryRecord>) {
    collected_original(test_support::instance_resource_evidence(case)).await
}

async fn collected_original(
    (source, rows): (CollectionDescription, Vec<TelemetryRecord>),
) -> (Fixture, CollectionDescription, Vec<TelemetryRecord>) {
    let catalog = Arc::new(SchemaCatalog::new().unwrap());
    let recovery =
        test_support::collected_resource_recovery("adx:test", catalog.digest(), source.clone(), rows.clone()).await;
    (Fixture::from_recovery(catalog, Arc::new(recovery)).await, source, rows)
}

#[tokio::test]
async fn collected_sealed_restarted_published_instance_points_preserve_exact_native_scope_and_all_fields() {
    for case in InstanceResourceCase::ALL {
        let (fixture, source, rows) = collected(case).await;
        fixture.replies(if fixture.rows.is_empty() { vec![] } else { vec![fixture.rows.clone()] });
        let opened = fixture.recovery.object_reads.load(Ordering::SeqCst);
        let capture = fixture
            .reader()
            .read_capture(&fixture.range, &[EvidenceFamily::OperationalSamples], EvidenceReadBudget::default(), None)
            .await
            .unwrap();
        test_support::assert_operational_capture(&capture, &source, &rows);
        assert!(capture.is_read_complete());
        assert!(capture.source().target.database().is_none());
        assert_eq!(capture.source().partition, source.partition);
        assert_eq!(capture.captured_rows(TelemetryStream::OperationalSamples), Some(rows.len() as u32));
        for (actual, native) in capture.records().iter().zip(&rows) {
            assert_eq!(actual.row(), &ExportedRecord::new(&source, capture.capture(), native.clone()).unwrap());
        }
        assert_eq!(capture.metrics().recovery_objects, 0);
        assert_eq!(fixture.recovery.object_reads.load(Ordering::SeqCst), opened);
    }
}

#[tokio::test]
async fn absent_instance_bodies_preserve_original_counts_without_recovery_hydration_or_native_fallback() {
    let (fixture, source, _) = collected(InstanceResourceCase::Exact).await;
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
    assert!(capture.records().is_empty());
    assert_eq!(capture.captured_rows(TelemetryStream::OperationalSamples), Some(1));
    assert_eq!(capture.source(), &source);
    assert!(capture.gaps().iter().any(|gap| gap.reason == EvidenceGapReason::MissingRows));
    assert_eq!(capture.metrics().queries, 3, "one publication, one membership audit and one bounded ordinal page");
    assert_eq!(capture.metrics().pages, 1);
    assert_eq!(fixture.query.requests.lock().unwrap().len(), 3);
    assert!(fixture.query.replies.lock().unwrap().is_empty());
    assert_eq!(capture.metrics().recovery_objects, 0);
    assert_eq!(fixture.recovery.object_reads.load(Ordering::SeqCst), opened);
}

#[tokio::test]
async fn changed_point_clock_population_flag_exact_value_or_null_cannot_borrow_an_original_receipt() {
    for (pointer, value) in [
        ("/observed_at", json!("2020-01-01T12:00:00.000000006Z")),
        ("/schedulers/population_rows", json!(3)),
        ("/grant_rows", json!(1)),
        ("/memory/process_physical_low", json!(true)),
        ("/memory/physical_used_kib", json!("9007199254740992")),
        ("/memory/physical_used_kib", Value::Null),
    ] {
        let (fixture, _, _) = collected(InstanceResourceCase::Exact).await;
        let schema = fixture.catalog.get(TelemetryStream::OperationalSamples).unwrap();
        let column = schema.columns().iter().position(|column| column.name == "Details").unwrap();
        let mut rows = fixture.rows.clone();
        let mut detail: Value = serde_json::from_str(rows[0][column].as_str().unwrap()).unwrap();
        *detail.pointer_mut(pointer).unwrap() = value;
        rows[0][column] = json!(serde_json::to_string(&detail).unwrap());
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
}

#[tokio::test]
async fn process_cpu_counter_sets_replay_original_nullable_totals_source_clock_and_process_without_recovery() {
    for case in test_support::ProcessCpuCase::ALL {
        let (fixture, source, rows) = collected_original(test_support::instance_resource_cpu_evidence(case)).await;
        fixture.replies(vec![fixture.rows.clone()]);
        let opened = fixture.recovery.object_reads.load(Ordering::SeqCst);
        let capture = fixture
            .reader()
            .read_capture(&fixture.range, &[EvidenceFamily::OperationalSamples], EvidenceReadBudget::default(), None)
            .await
            .unwrap();
        test_support::assert_operational_capture(&capture, &source, &rows);
        assert!(capture.is_read_complete());
        assert_eq!(capture.source(), &source);
        let context = NativeReadContext::from_snapshot(&source).unwrap();
        let mut points = 0;
        for row in capture.records() {
            if let TelemetryRecord::Operational(observation) = &row.row().record
                && let OperationalFact::CounterSet(sample) = &observation.fact
            {
                let point = MssqlProcessCpuPoint::from_original(sample, &context).unwrap();
                assert_eq!(point.observed_at(), "2020-01-01T12:00:00.5Z");
                if case == test_support::ProcessCpuCase::Null {
                    assert!(point.user_ms().is_none() && point.kernel_ms().is_none());
                }
                if case == test_support::ProcessCpuCase::Exact {
                    assert_eq!(point.user_ms(), Some("9007199254740993"));
                }
                points += 1;
            }
        }
        assert_eq!(points, usize::from(case != test_support::ProcessCpuCase::Denied));
        assert_eq!(fixture.recovery.object_reads.load(Ordering::SeqCst), opened);
        assert_eq!(capture.metrics().recovery_objects, 0);
    }
}

#[tokio::test]
async fn individual_schedulers_replay_the_same_original_identity_nullable_fields_units_and_complete_source_coverage() {
    // A real caller may choose a smaller ordinal page. Fixed fake replies must fit both reader row and
    // byte admission; a default250-row chunk is not an authoritative byte-admitted window for these rows.
    let read_budget = EvidenceReadBudget { page_rows: 3, ..EvidenceReadBudget::default() };
    for case in test_support::SchedulerCase::ALL {
        let (fixture, source, rows) =
            collected_original(test_support::instance_resource_scheduler_evidence(case)).await;
        fixture.replies(fixture.rows.chunks(read_budget.page_rows as usize).map(|rows| rows.to_vec()).collect());
        let opened = fixture.recovery.object_reads.load(Ordering::SeqCst);
        let capture = fixture
            .reader()
            .read_capture(&fixture.range, &[EvidenceFamily::OperationalSamples], read_budget, None)
            .await
            .unwrap_or_else(|error| panic!("{case:?}: {error:?}"));
        test_support::assert_operational_capture(&capture, &source, &rows);
        assert!(capture.is_read_complete());
        let expected_pages = rows.len().div_ceil(read_budget.page_rows as usize) as u32;
        assert_eq!(capture.metrics().pages, expected_pages, "{case:?}");
        assert_eq!(capture.metrics().queries, expected_pages + 2, "publication + membership + data pages");
        assert_eq!(capture.metrics().returned_rows, rows.len() as u32, "{case:?}");
        assert!(capture.metrics().queries <= read_budget.max_queries);
        assert!(fixture.query.replies.lock().unwrap().is_empty(), "{case:?}: every planned page consumed");
        let native = source
            .observation
            .snapshot()
            .unwrap()
            .sources
            .iter()
            .find(|source| source.source == culprit_core::source_id::SCHEDULERS)
            .unwrap();
        let restored = capture
            .records()
            .iter()
            .filter_map(|row| match &row.row().record {
                TelemetryRecord::Operational(observation)
                    if observation.source == culprit_core::source_id::SCHEDULERS =>
                {
                    let OperationalFact::CounterSet(sample) = &observation.fact else { panic!() };
                    Some(sample.as_ref())
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(restored.len(), native.records as usize);
        for sample in restored {
            assert!(MssqlSchedulerPoint::from_sample(sample).is_ok());
        }
        assert_eq!(capture.source(), &source);
        assert_eq!(fixture.recovery.object_reads.load(Ordering::SeqCst), opened);
        assert_eq!(capture.metrics().recovery_objects, 0);
    }
}

#[tokio::test]
async fn changed_scheduler_identity_status_null_or_exact_counter_cannot_borrow_its_original_receipt() {
    for (pointer, value) in [
        ("/identity/id", json!("99")),
        ("/properties/status", json!("VISIBLE OFFLINE")),
        ("/counters/0/value/value", json!("3")),
        ("/properties/native_null_columns", json!("[\"current_tasks_count\"]")),
    ] {
        let (fixture, _, _) = collected_original(test_support::instance_resource_scheduler_evidence(
            test_support::SchedulerCase::NativeNull,
        ))
        .await;
        let schema = fixture.catalog.get(TelemetryStream::OperationalSamples).unwrap();
        let column = schema.columns().iter().position(|column| column.name == "Details").unwrap();
        let mut rows = fixture.rows.clone();
        let mut details: Value = serde_json::from_str(rows[1][column].as_str().unwrap()).unwrap();
        *details.pointer_mut(pointer).unwrap() = value;
        rows[1][column] = json!(serde_json::to_string(&details).unwrap());
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
}

#[tokio::test]
async fn original_numa_node_fields_replay_from_the_same_canonical_store_source_without_recovery() {
    for case in test_support::NumaNodeCase::ALL {
        let (fixture, source, rows) = collected_original(test_support::numa_node_evidence(case)).await;
        let read_budget = EvidenceReadBudget { page_rows: 3, ..EvidenceReadBudget::default() };
        fixture.replies(fixture.rows.chunks(read_budget.page_rows as usize).map(<[_]>::to_vec).collect());
        let opened = fixture.recovery.object_reads.load(Ordering::SeqCst);
        let capture = fixture
            .reader()
            .read_capture(&fixture.range, &[EvidenceFamily::OperationalSamples], read_budget, None)
            .await
            .unwrap_or_else(|error| panic!("{case:?}: {error}"));
        test_support::assert_operational_capture(&capture, &source, &rows);
        assert_eq!(capture.source().partition, source.partition);
        assert_eq!(capture.metrics().recovery_objects, 0);
        assert_eq!(fixture.recovery.object_reads.load(Ordering::SeqCst), opened);
        for (actual, original) in capture.records().iter().zip(&rows) {
            assert_eq!(actual.row(), &ExportedRecord::new(&source, capture.capture(), original.clone()).unwrap());
        }
    }
}
