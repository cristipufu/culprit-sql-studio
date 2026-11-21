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
