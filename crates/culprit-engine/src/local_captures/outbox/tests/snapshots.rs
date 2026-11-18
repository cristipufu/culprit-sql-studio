use super::*;
use culprit_collector::analytical::test_support::*;
use culprit_core::SourceState;

pub(super) fn originals(kind: LocalCaptureAnalysisKind) -> (CollectionDescription, Vec<TelemetryRecord>) {
    let name = serde_json::to_value(kind).unwrap();
    let case = local_snapshot_evidence_cases()
        .into_iter()
        .find(|case| name == case.analysis)
        .expect("every generic snapshot selector has one original corpus case");
    (case.source, case.records)
}
async fn analyze(
    reader: &OutboxCaptureReader,
    id: &str,
    kind: LocalCaptureAnalysisKind,
) -> Result<LocalSnapshotAnalysis> {
    let guard = OperationGuard::new(reader.timeout()).unwrap();
    let result = reader
        .analyze(LocalCaptureRequest {
            connection: "local-evidence",
            observation_id: id,
            analysis: kind,
            operation: guard.handle(),
        })
        .await?;
    assert_eq!(result.kind(), kind);
    let LocalCapturedAnalysis::Snapshot(result) = result else { panic!("one generic Operational result") };
    Ok(result)
}
fn reader(fixture: &LocalCaptureFixture, budget: CompletedCaptureBudget) -> OutboxCaptureReader {
    OutboxCaptureReader::delivery(fixture.outbox.as_ref().unwrap().delivery(), budget).unwrap()
}

#[tokio::test]
async fn every_new_snapshot_report_retains_one_exact_original_without_source_work_or_scope_relabeling() {
    for kind in LocalCaptureAnalysisKind::ALL.into_iter().filter(|kind| {
        !matches!(
            kind,
            LocalCaptureAnalysisKind::PageServerReads
                | LocalCaptureAnalysisKind::MemoryClerks
                | LocalCaptureAnalysisKind::HyperscaleLogComponents
        )
    }) {
        let (source, rows) = originals(kind);
        let fixture = LocalCaptureFixture::snapshot(source, rows).await;
        assert_eq!(fixture.manifest.status, CollectionStatus::Complete, "{kind:?}");
        let result = analyze(
            &reader(&fixture, CompletedCaptureBudget::default()),
            &fixture.manifest.capture.observation_id,
            kind,
        )
        .await
        .unwrap();
        let original = result.original();
        let read = original.read();
        assert_eq!(&read.description, fixture.manifest.description.as_ref().unwrap(), "{kind:?}");
        assert_eq!(original.connection(), "local-evidence");
        assert_eq!(original.collected_from(), fixture.manifest.started_at);
        assert_eq!(Some(original.collected_until()), fixture.manifest.finished_at.as_deref());
        assert_eq!(original.bytes_read(), fixture.original_bytes());
        assert!(read.read_complete && read.is_valid());
        assert_eq!(read.origin.capture.capture, fixture.manifest.capture);
        assert_eq!(read.origin.positions.len(), read.observations.len());
        assert_eq!(read.context().unwrap().partition, read.description.partition);
        assert_eq!(fixture.manifest.source_queries, 0, "controlled source is not native cost qualification");
        assert!(
            read.captured_references().unwrap().iter().all(|reference| reference.capture_ref() == read.origin.capture)
        );
        if let LocalSnapshotReport::Compilation(report) = result.report() {
            assert_eq!(
                read.context().unwrap().partition,
                SourcePartition::Target,
                "query-global data namespace is not the compiler execution owner"
            );
            assert!(report.comparison.is_none() && report.definition_read.is_none());
            assert!(!report.entries.is_empty());
        }
    }
}

#[tokio::test]
async fn native_counter_and_database_property_visibility_remain_separate_from_fully_read_local_bodies() {
    for case in PerformanceCounterCase::ALL {
        let (source, rows) = performance_counter_evidence(case);
        let expected = source.observation.snapshot().unwrap().sources[0].clone();
        let fixture = LocalCaptureFixture::snapshot(source, rows).await;
        let result = analyze(
            &reader(&fixture, CompletedCaptureBudget::default()),
            &fixture.manifest.capture.observation_id,
            LocalCaptureAnalysisKind::PerformanceCounters,
        )
        .await
        .unwrap();
        let LocalSnapshotReport::PerformanceCounters(report) = result.report() else { panic!() };
        assert_eq!(report.native_source.as_ref(), Some(&expected));
        assert_eq!(report.source_state, Some(expected.state));
        assert_eq!(report.read_complete, Some(true));
        assert_eq!(result.original().read().source_coverage("independent-native-counters").unwrap().records, 1);
        assert_eq!(report.observed_observations, expected.records);
        if case == PerformanceCounterCase::Denied {
            assert!(report.entries.is_empty());
        }
        if case == PerformanceCounterCase::Exact {
            assert!(report.entries.iter().any(|entry| entry.raw_value == "9007199254740993"));
            assert_eq!(
                report.native_counter_rows,
                Some(11),
                "actual retained rows, independently of the larger fixed source selection"
            );
        }
    }
    for case in DatabasePropertiesCase::ALL {
        let (source, rows) = database_properties_evidence(case);
        let expected = source.observation.snapshot().unwrap().sources[0].state;
        let fixture = LocalCaptureFixture::snapshot(source, rows).await;
        let result = analyze(
            &reader(&fixture, CompletedCaptureBudget::default()),
            &fixture.manifest.capture.observation_id,
            LocalCaptureAnalysisKind::DatabaseProperties,
        )
        .await
        .unwrap();
        let LocalSnapshotReport::DatabaseProperties(report) = result.report() else { panic!() };
        assert_eq!(report.source_state, Some(expected));
        assert_eq!(report.read_complete, Some(true));
        if expected == SourceState::Unavailable {
            assert!(report.fields.is_empty());
        }
    }
}

#[tokio::test]
async fn same_stream_does_not_authorize_a_different_family_or_omit_its_declared_source() {
    let (source, rows) = storage_evidence(StorageCase::Exact);
    let fixture = LocalCaptureFixture::snapshot(source, rows).await;
    let reader = reader(&fixture, CompletedCaptureBudget::default());
    for kind in [
        LocalCaptureAnalysisKind::PerformanceCounters,
        LocalCaptureAnalysisKind::DatabaseProperties,
        LocalCaptureAnalysisKind::ConfiguredLogRates,
        LocalCaptureAnalysisKind::Compilation,
    ] {
        assert_eq!(
            analyze(&reader, &fixture.manifest.capture.observation_id, kind).await.unwrap_err().detail,
            Some(ErrorDetail::Delivery(DeliveryProblem::UnsupportedSchema)),
            "{kind:?}"
        );
    }
    assert!(
        analyze(&reader, &fixture.manifest.capture.observation_id, LocalCaptureAnalysisKind::Storage).await.is_ok()
    );
}

#[tokio::test]
async fn snapshot_budget_boundaries_refuse_a_missing_original_body_and_allow_the_exact_verified_bytes() {
    let (source, rows) = performance_counter_evidence(PerformanceCounterCase::Exact);
    let fixture = LocalCaptureFixture::snapshot(source, rows).await;
    for budget in [
        CompletedCaptureBudget { max_rows: 1, ..Default::default() },
        CompletedCaptureBudget { max_bytes: fixture.original_bytes() - 1, ..Default::default() },
    ] {
        let error = analyze(
            &reader(&fixture, budget),
            &fixture.manifest.capture.observation_id,
            LocalCaptureAnalysisKind::PerformanceCounters,
        )
        .await
        .unwrap_err();
        assert_eq!(error.detail, Some(ErrorDetail::Delivery(DeliveryProblem::Capacity)));
    }
    let result = analyze(
        &reader(&fixture, CompletedCaptureBudget { max_bytes: fixture.original_bytes(), ..Default::default() }),
        &fixture.manifest.capture.observation_id,
        LocalCaptureAnalysisKind::PerformanceCounters,
    )
    .await
    .unwrap();
    assert_eq!(result.original().bytes_read(), fixture.original_bytes());
}

#[tokio::test]
async fn cold_snapshot_reads_preserve_original_proofs_and_reject_tampered_native_values() {
    let (source, rows) = performance_counter_evidence(PerformanceCounterCase::Exact);
    let mut fixture = LocalCaptureFixture::snapshot(source, rows).await;
    let before = analyze(
        &reader(&fixture, CompletedCaptureBudget::default()),
        &fixture.manifest.capture.observation_id,
        LocalCaptureAnalysisKind::PerformanceCounters,
    )
    .await
    .unwrap();
    let bytes = serde_json::to_vec(before.original().read()).unwrap();
    drop(fixture.outbox.take());
    let reader = OutboxCaptureReader::directory(fixture.directory.clone(), CompletedCaptureBudget::default()).unwrap();
    let restored =
        analyze(&reader, &fixture.manifest.capture.observation_id, LocalCaptureAnalysisKind::PerformanceCounters)
            .await
            .unwrap();
    assert_eq!(serde_json::to_vec(restored.original().read()).unwrap(), bytes);
    let path = fixture.capture_directory.join(&fixture.manifest.files[0].file);
    let original = std::fs::read_to_string(&path).unwrap();
    let changed = original.replace("9007199254740993", "9007199254740994");
    assert_ne!(changed, original);
    std::fs::write(path, changed).unwrap();
    assert!(
        analyze(&reader, &fixture.manifest.capture.observation_id, LocalCaptureAnalysisKind::PerformanceCounters)
            .await
            .is_err()
    );
}
