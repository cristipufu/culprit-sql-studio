use super::*;
use serde_json::Value;
use std::sync::atomic::Ordering;

async fn collected(source: CollectionDescription, records: Vec<TelemetryRecord>, family: EvidenceFamily) -> Fixture {
    let catalog = Arc::new(SchemaCatalog::new().unwrap());
    let recovery =
        test_support::collected_resource_recovery("clickhouse:test", catalog.digest(), source, records).await;
    Fixture::from_recovery(catalog, Arc::new(recovery), &[family.stream()]).await
}

#[tokio::test]
async fn real_capture_seal_restart_commit_and_projection_readback_preserve_accounting_and_native_wait_mechanisms() {
    let mut cases: Vec<_> = [DataIoAccounting::DataFiles, DataIoAccounting::ComputeLocalSsd, DataIoAccounting::Unknown]
        .into_iter()
        .map(|basis| {
            let (source, records) = test_support::resource_io_evidence(basis);
            (source, records, EvidenceFamily::ResourceSamples)
        })
        .collect();
    let (source, records) = test_support::rbio_wait_evidence();
    cases.push((source, records, EvidenceFamily::WaitCounterSamples));
    for (source, records, family) in cases {
        let fixture = collected(source.clone(), records.clone(), family).await;
        let opened = fixture.recovery.object_reads.load(Ordering::SeqCst);
        fixture.replies(vec![fixture.rows.clone()]);
        let capture =
            fixture.reader.read_capture(&fixture.range, &[family], EvidenceReadBudget::default(), None).await.unwrap();
        assert!(capture.is_read_complete(), "{:?}", capture.gaps());
        assert_eq!(capture.source(), &source);
        assert_eq!(capture.records().len(), records.len());
        assert_eq!(capture.captured_rows(family.stream()), Some(records.len() as u32));
        assert_eq!(capture.metrics().recovery_objects, 0);
        for (actual, native) in capture.records().iter().zip(&records) {
            let original = ExportedRecord::new(&source, capture.capture(), native.clone()).unwrap();
            assert_eq!(actual.row(), &original);
            if let TelemetryRecord::WaitCounter(point) = &actual.row().record {
                assert_eq!(point.waiting_tasks_count, "9007199254740993");
                assert_eq!(point.wait_time_ms, "9007199254740993");
                assert_eq!(point.signal_wait_time_ms, "0");
                assert_eq!(point.reset, culprit_core::ResetMarker::Unknown);
                assert_eq!(point.mechanism, culprit_core::Engine::Mssql.spec().native_wait_mechanism(&point.wait_type));
            }
        }
        assert_eq!(fixture.recovery.object_reads.load(Ordering::SeqCst), opened);
    }
}

#[tokio::test]
async fn changed_accounting_mechanism_exact_value_scope_or_null_projection_fails_original_receipts() {
    for mutation in ["accounting", "mechanism", "null-mechanism", "counter", "scope"] {
        let resource = mutation == "accounting";
        let (source, records) = if resource {
            test_support::resource_io_evidence(DataIoAccounting::ComputeLocalSsd)
        } else {
            test_support::rbio_wait_evidence()
        };
        let family = if resource { EvidenceFamily::ResourceSamples } else { EvidenceFamily::WaitCounterSamples };
        let fixture = collected(source, records, family).await;
        let opened = fixture.recovery.object_reads.load(Ordering::SeqCst);
        let schema = fixture.catalog.get(family.stream()).unwrap();
        let index = |name: &str| schema.columns().iter().position(|column| column.name == name).unwrap();
        let mut rows = fixture.rows.clone();
        match mutation {
            "accounting" => rows[0][index("DataIoAccounting")] = json!("data-files"),
            "mechanism" => {
                rows[0][index("WaitMechanism")] = json!(
                    serde_json::to_string(&culprit_core::Engine::Mssql.spec().native_wait_mechanism("RBIO_RG_REPLICA"))
                        .unwrap()
                )
            }
            "null-mechanism" => {
                rows[0][index("WaitMechanism")] = Value::Null;
            }
            "counter" => rows[0][index("WaitTimeMs")] = json!("9007199254740992"),
            "scope" => rows[0][index("MeasurementScope")] = json!("server"),
            _ => unreachable!(),
        }
        fixture.replies(vec![rows]);
        let error = fixture
            .reader
            .read_capture(&fixture.range, &[family], EvidenceReadBudget::default(), Some(fixture.recovery.as_ref()))
            .await
            .unwrap_err();
        assert_eq!(error.detail, Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::Integrity)), "{mutation}");
        assert_eq!(fixture.recovery.object_reads.load(Ordering::SeqCst), opened);
    }
}

#[tokio::test]
async fn missing_wait_body_preserves_original_native_counts_and_bounds_as_an_explicit_partial_read() {
    let (source, records) = test_support::rbio_wait_evidence();
    let family = EvidenceFamily::WaitCounterSamples;
    let fixture = collected(source.clone(), records.clone(), family).await;
    let mut rows = fixture.rows.clone();
    rows.remove(0);
    fixture.replies(vec![rows]);
    let capture =
        fixture.reader.read_capture(&fixture.range, &[family], EvidenceReadBudget::default(), None).await.unwrap();
    assert!(!capture.is_read_complete());
    assert_eq!(capture.source(), &source);
    assert_eq!(capture.captured_rows(family.stream()), Some(records.len() as u32));
    assert_eq!(capture.records().len(), records.len() - 1);
    assert!(capture.gaps().iter().any(|gap| gap.reason == EvidenceGapReason::MissingRows));
    let rows = capture
        .records()
        .iter()
        .map(|record| {
            let TelemetryRecord::WaitCounter(row) = &record.row().record else { panic!() };
            row.clone()
        })
        .collect();
    let read = WaitCounterSnapshot::from_source(capture.source(), rows, capture.is_read_complete()).unwrap();
    assert!(!read.read_complete);
    assert_eq!(read.sources, source.observation.snapshot().unwrap().sources);
    assert_eq!(read.sources[0].records, records.len() as u32);
    assert_eq!(read.observations.len(), records.len() - 1);
    assert_eq!(capture.metrics().recovery_objects, 0);
}

#[tokio::test]
async fn an_unavailable_native_wait_source_remains_unavailable_after_a_complete_archive_read() {
    let (mut source, _) = test_support::rbio_wait_evidence();
    let SourceObservation::Snapshot(snapshot) = &mut source.observation else { unreachable!() };
    snapshot.records = 0;
    snapshot.sources[0] = StateSourceCoverage::unavailable(
        culprit_core::source_id::WAITS.into(),
        culprit_core::ErrorCode::PermissionDenied,
        None,
    );
    let fixture = collected(source.clone(), vec![], EvidenceFamily::WaitCounterSamples).await;
    fixture.replies(vec![]);
    let capture = fixture
        .reader
        .read_capture(&fixture.range, &[EvidenceFamily::WaitCounterSamples], EvidenceReadBudget::default(), None)
        .await
        .unwrap();
    assert!(capture.is_read_complete());
    assert!(capture.records().is_empty());
    assert_eq!(capture.source(), &source);
    let native = capture.source().observation.snapshot().unwrap();
    assert_eq!(native.sources[0].state, culprit_core::SourceState::Unavailable);
    assert_eq!(native.sources[0].error_code, Some(culprit_core::ErrorCode::PermissionDenied));
    assert!(!native.sources[0].complete_scope);
}

#[tokio::test]
async fn genuine_resource_role_capture_seal_restart_and_readback_preserve_all_dimensions_without_member_invention() {
    for (source, records) in test_support::resource_role_evidence_cases() {
        let fixture = collected(source.clone(), records.clone(), EvidenceFamily::ResourceSamples).await;
        let opened = fixture.recovery.object_reads.load(Ordering::SeqCst);
        fixture.replies(vec![fixture.rows.clone()]);
        let capture = fixture
            .reader
            .read_capture(&fixture.range, &[EvidenceFamily::ResourceSamples], EvidenceReadBudget::default(), None)
            .await
            .unwrap();
        assert!(capture.is_read_complete());
        assert_eq!(capture.source(), &source);
        assert_eq!(capture.captured_rows(TelemetryStream::ResourceSamples), Some(1));
        assert_eq!(capture.records().len(), 1);
        let expected = ExportedRecord::new(&source, capture.capture(), records[0].clone()).unwrap();
        assert_eq!(capture.records()[0].row(), &expected);
        let TelemetryRecord::ResourceSample(point) = &capture.records()[0].row().record else { panic!() };
        assert_eq!(point.replica_id, None);
        assert_eq!(point.metrics.len(), 11);
        assert_eq!(point.metrics[0].value, Some(0.0));
        assert_eq!(point.metrics[3].value, None);
        assert_eq!(point.metrics[7].scope, culprit_core::MeasurementScope::Server);
        assert_eq!(capture.metrics().queries, 3);
        assert_eq!(capture.metrics().recovery_objects, 0);
        assert_eq!(fixture.recovery.object_reads.load(Ordering::SeqCst), opened);
    }
}

#[tokio::test]
async fn changed_role_presence_identity_or_dimensions_fail_original_resource_projection_proof() {
    for mutation in ["role", "role-presence", "member", "metrics"] {
        let (source, records) = test_support::resource_role_evidence_cases().remove(1);
        let fixture = collected(source.clone(), records.clone(), EvidenceFamily::ResourceSamples).await;
        let opened = fixture.recovery.object_reads.load(Ordering::SeqCst);
        let schema = fixture.catalog.get(TelemetryStream::ResourceSamples).unwrap();
        let index = |name: &str| schema.columns().iter().position(|column| column.name == name).unwrap();
        let mut rows = fixture.rows.clone();
        match mutation {
            "role" => {
                rows[0][index("ReplicaRole")] = json!(
                    serde_json::to_string(&NativeResourceReplicaRole::MssqlDbResourceStats(
                        MssqlResourceReplicaRole::NamedReplica
                    ))
                    .unwrap()
                )
            }
            "role-presence" => rows[0][index("ReplicaRole")] = Value::Null,
            "member" => rows[0][index("ReplicaId")] = json!("0"),
            "metrics" => {
                let mut metrics: Value = serde_json::from_str(rows[0][index("Metrics")].as_str().unwrap()).unwrap();
                metrics[7]["scope"] = json!("database");
                rows[0][index("Metrics")] = json!(metrics.to_string());
            }
            _ => unreachable!(),
        }
        fixture.replies(vec![rows]);
        let error = fixture
            .reader
            .read_capture(
                &fixture.range,
                &[EvidenceFamily::ResourceSamples],
                EvidenceReadBudget::default(),
                Some(fixture.recovery.as_ref()),
            )
            .await
            .unwrap_err();
        assert_eq!(error.detail, Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::Integrity)), "{mutation}");
        assert_eq!(fixture.recovery.object_reads.load(Ordering::SeqCst), opened);
    }
}

#[tokio::test]
async fn absent_resource_body_retains_original_count_role_gap_and_never_hydrates_a_new_point() {
    let (source, records) = test_support::resource_role_evidence_cases().remove(1);
    let fixture = collected(source.clone(), records, EvidenceFamily::ResourceSamples).await;
    let opened = fixture.recovery.object_reads.load(Ordering::SeqCst);
    fixture.replies(vec![vec![]]);
    let capture = fixture
        .reader
        .read_capture(
            &fixture.range,
            &[EvidenceFamily::ResourceSamples],
            EvidenceReadBudget::default(),
            Some(fixture.recovery.as_ref()),
        )
        .await
        .unwrap();
    assert!(!capture.is_read_complete());
    assert!(capture.records().is_empty());
    assert_eq!(capture.source(), &source);
    assert_eq!(capture.captured_rows(TelemetryStream::ResourceSamples), Some(1));
    assert!(capture.gaps().iter().any(|gap| gap.reason == EvidenceGapReason::MissingRows));
    assert_eq!(capture.metrics().queries, 3);
    assert_eq!(capture.metrics().recovery_objects, 0);
    assert_eq!(fixture.recovery.object_reads.load(Ordering::SeqCst), opened);
}

#[tokio::test]
async fn exact_wait_carriers_preserve_all_native_rows_and_independent_visibility_through_real_capture_and_store() {
    for case in test_support::WaitCounterCase::ALL {
        let (source, records) = test_support::wait_counter_evidence(case);
        let family = EvidenceFamily::WaitCounterSamples;
        let fixture = collected(source.clone(), records.clone(), family).await;
        let opened = fixture.recovery.object_reads.load(Ordering::SeqCst);
        fixture.replies(vec![fixture.rows.clone()]);
        let capture =
            fixture.reader.read_capture(&fixture.range, &[family], EvidenceReadBudget::default(), None).await.unwrap();
        assert!(capture.is_read_complete(), "{case:?}");
        let rows = capture
            .records()
            .iter()
            .map(|record| {
                let TelemetryRecord::WaitCounter(row) = &record.row().record else { panic!() };
                row.clone()
            })
            .collect();
        let read = WaitCounterSnapshot::from_source(capture.source(), rows, capture.is_read_complete()).unwrap();
        assert!(read.is_valid_for(source.target.engine()));
        assert_eq!(read.sources, source.observation.snapshot().unwrap().sources);
        assert_eq!(read.observations.len(), records.len());
        assert_eq!(
            read.context.as_ref().unwrap().bounds(),
            ("2020-01-01T12:00:00.000000001Z", "2020-01-01T12:00:00.000000009Z")
        );
        for (actual, original) in capture.records().iter().zip(&records) {
            assert_eq!(actual.row(), &ExportedRecord::new(&source, capture.capture(), original.clone()).unwrap());
        }
        if case == test_support::WaitCounterCase::InconsistentSignal {
            assert_eq!(read.observations[0].resource_wait_time_ms(), None);
            assert_eq!(read.observations[0].wait_time_ms, "1");
            assert_eq!(read.observations[0].signal_wait_time_ms, "2");
        }
        assert_eq!(fixture.recovery.object_reads.load(Ordering::SeqCst), opened);
        assert_eq!(capture.metrics().recovery_objects, 0);
    }
}
