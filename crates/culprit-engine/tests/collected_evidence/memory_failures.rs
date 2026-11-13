use super::fixture::Fixture;
use culprit_collector::analytical::{evidence::EvidenceFamily, test_support::memory_failure_evidence};
use culprit_core::{
    Engine, Snapshot, SourceOutcome, SourceState, operations::MemoryFailureSnapshot, source_id, telemetry::*,
};
use culprit_engine::collected_resources;

fn native(source: &mut CollectionDescription) -> &mut StateSnapshotDescription {
    let SourceObservation::Snapshot(snapshot) = &mut source.observation else { panic!("snapshot fixture") };
    snapshot
}

fn scan(source: &CollectionDescription, records: &[TelemetryRecord]) -> Snapshot {
    let CollectionTarget::Shared(resource) = &source.target else { panic!("shared fixture") };
    let SourceObservation::Snapshot(native) = &source.observation else { panic!("snapshot fixture") };
    let mut scan: Snapshot = serde_json::from_str(include_str!("../../../../fixtures/mssql-demo.json")).unwrap();
    scan.operations.memory_failures = Some(MemoryFailureSnapshot {
        source: native.sources[0].source.clone(),
        observed_from: native.observed_from.clone(),
        observed_until: native.observed_until.clone(),
        scope: source.target.measurement_scope(),
        resource: Some(resource.clone()),
        observations: records
            .iter()
            .map(|record| {
                let TelemetryRecord::Operational(observation) = record else { panic!("native fixture") };
                let OperationalFact::MssqlOutOfMemory(failure) = &observation.fact else { panic!("OOM fixture") };
                failure.as_ref().clone()
            })
            .collect(),
    });
    scan.sources.retain(|coverage| coverage.source != native.sources[0].source);
    scan.sources.push(SourceOutcome {
        source: native.sources[0].source.clone(),
        state: native.sources[0].state,
        reason: None,
        reason_message: None,
    });
    scan
}

#[tokio::test]
async fn authenticated_memory_failures_reuse_scan_explanations_without_inventing_a_database_or_owner() {
    let (source, records) = memory_failure_evidence();
    let expected = culprit_checks::analyze(scan(&source, &records)).memory_failures.unwrap();
    let fixture = Fixture::new(source.clone(), records.clone()).await;
    let capture = fixture.read_families(&[EvidenceFamily::OperationalSamples]).await;
    assert!(capture.is_read_complete(), "{:?}", capture.gaps());
    let analysis = collected_resources::memory_failures(&capture).unwrap();
    assert_eq!(serde_json::to_value(&analysis.report).unwrap(), serde_json::to_value(expected).unwrap());
    assert_eq!(analysis.context.source, source);
    assert!(analysis.context.source.target.database().is_none());
    assert_eq!(analysis.context.manifest_sha256, fixture.range.manifest_digest());
    assert_eq!(analysis.context.selected_streams, [TelemetryStream::OperationalSamples]);
    assert!(analysis.context.read_gaps.is_empty());
    assert_eq!(analysis.context.records.len(), 2);
    for record in records {
        assert!(analysis.context.records.iter().any(|row| row.row.record == record));
    }
    assert_eq!(analysis.report.entries.iter().map(|entry| entry.position).collect::<Vec<_>>(), [0, 1]);
    assert!(!serde_json::to_string(&analysis.report).unwrap().contains("future_member"), "raw evidence has one owner");
}

#[tokio::test]
async fn physical_batch_order_does_not_change_capture_positions_or_compare_native_clock_with_utc() {
    let (source, mut records) = memory_failure_evidence();
    let TelemetryRecord::Operational(observation) = &mut records[0] else { panic!() };
    let OperationalFact::MssqlOutOfMemory(failure) = &mut observation.fact else { panic!() };
    failure.event_time_without_offset = "2099-01-01T00:00:00.003".into();
    records.reverse();
    let fixture = Fixture::new(source, records).await;
    let capture = fixture.read_families(&[EvidenceFamily::OperationalSamples]).await;
    let analysis = collected_resources::memory_failures(&capture).unwrap();
    assert_eq!(analysis.report.entries.iter().map(|entry| entry.position).collect::<Vec<_>>(), [0, 1]);
    assert!(analysis.context.records.iter().any(|record| {
        matches!(&record.row.record, TelemetryRecord::Operational(OperationalObservation {
            fact: OperationalFact::MssqlOutOfMemory(failure), ..
        }) if failure.event_time_without_offset == "2099-01-01T00:00:00.003")
    }));
}

#[tokio::test]
async fn partial_store_read_keeps_positive_rows_and_native_source_loss_separate() {
    let (source, records) = memory_failure_evidence();
    let fixture = Fixture::new(source, records).await;
    fixture.remove_first_row(TelemetryStream::OperationalSamples);
    let capture = fixture.read_families(&[EvidenceFamily::OperationalSamples]).await;
    assert!(!capture.is_read_complete());
    let analysis = collected_resources::memory_failures(&capture).unwrap();
    assert_eq!(analysis.report.source_state, Some(SourceState::Partial));
    assert!(analysis.report.source_reason.is_some());
    assert_eq!(analysis.report.entries.len(), 1);
    assert_eq!(analysis.report.entries[0].position, 1);
    assert_eq!(analysis.context.records.len(), 1);
    assert!(
        analysis
            .context
            .read_gaps
            .iter()
            .any(|gap| gap.reason == culprit_core::collected_analysis::EvidenceGapReason::MissingRows)
    );
    let SourceObservation::Snapshot(native) = &analysis.context.source.observation else { panic!() };
    assert_eq!(native.sources[0].state, SourceState::Collected, "store loss cannot rewrite source collection history");
    assert_eq!(native.sources[0].records, 2);
}

#[tokio::test]
async fn unpublished_capture_does_not_produce_a_retained_failure_or_empty_history_claim() {
    let (source, records) = memory_failure_evidence();
    let fixture = Fixture::new(source, records).await;
    fixture.unpublish();
    let capture = fixture.read_families(&[EvidenceFamily::OperationalSamples]).await;
    let analysis = collected_resources::memory_failures(&capture).unwrap();
    assert_eq!(analysis.report.source_state, Some(SourceState::Partial));
    assert!(analysis.report.entries.is_empty());
    assert!(analysis.context.records.is_empty());
    assert!(
        analysis
            .context
            .read_gaps
            .iter()
            .any(|gap| gap.reason == culprit_core::collected_analysis::EvidenceGapReason::NotPublished)
    );
    assert!(analysis.report.source_reason.is_some());
}

#[tokio::test]
async fn empty_unavailable_and_partial_native_reads_preserve_their_qualified_state() {
    for state in [SourceState::Empty, SourceState::Unavailable, SourceState::Partial] {
        let (mut source, records) = memory_failure_evidence();
        let retained = if state == SourceState::Partial { records } else { vec![] };
        let snapshot = native(&mut source);
        snapshot.records = retained.len() as u32;
        snapshot.sources = vec![match state {
            SourceState::Unavailable => StateSourceCoverage::unavailable(
                source_id::MEMORY_FAILURES.into(),
                culprit_core::ErrorCode::PermissionDenied,
                None,
            ),
            _ => StateSourceCoverage::observed(
                source_id::MEMORY_FAILURES.into(),
                retained.len() as u32,
                state != SourceState::Partial,
            ),
        }];
        let fixture = Fixture::new(source, retained).await;
        let capture = fixture.read_families(&[EvidenceFamily::OperationalSamples]).await;
        assert!(capture.is_read_complete(), "{state:?}: {:?}", capture.gaps());
        let analysis = collected_resources::memory_failures(&capture).unwrap();
        assert_eq!(analysis.report.source_state, Some(state));
        assert_eq!(analysis.report.entries.len(), if state == SourceState::Partial { 2 } else { 0 });
        assert!(analysis.report.limitations.contains(&culprit_core::t!("view.memory_failures.limit.retention")));
    }
}

#[tokio::test]
async fn custom_source_identity_keeps_authenticated_native_coverage_without_an_engine_name_guess() {
    let (mut source, mut records) = memory_failure_evidence();
    source.source = "embedding-memory-observations".into();
    native(&mut source).sources[0].source = "embedding-native-memory-source".into();
    for record in &mut records {
        let TelemetryRecord::Operational(observation) = record else { panic!() };
        observation.source = "embedding-native-memory-source".into();
    }
    let expected = culprit_checks::analyze(scan(&source, &records)).memory_failures.unwrap();
    let fixture = Fixture::new(source.clone(), records).await;
    let capture = fixture.read_families(&[EvidenceFamily::OperationalSamples]).await;
    let analysis = collected_resources::memory_failures(&capture).unwrap();
    assert_eq!(analysis.context.source, source);
    assert_eq!(serde_json::to_value(analysis.report).unwrap(), serde_json::to_value(expected).unwrap());
}

#[tokio::test]
async fn other_subjects_and_datasets_cannot_acquire_native_memory_failure_semantics() {
    for shape in ["database", "node", "other-native-engine", "other-dataset"] {
        let (mut source, records) = memory_failure_evidence();
        match shape {
            "database" => {
                source.target =
                    culprit_core::DatabaseIdentity::server(Engine::Mssql, "db", "fixture.invalid", 1433, None).into()
            }
            "node" => {
                let CollectionTarget::Shared(resource) = &mut source.target else { panic!() };
                resource.kind = SharedResourceKind::Node;
            }
            "other-native-engine" => {
                let CollectionTarget::Shared(resource) = &mut source.target else { panic!() };
                resource.engine = Engine::Postgres;
            }
            "other-dataset" => native(&mut source).dataset = SnapshotDataset::Operational,
            _ => unreachable!(),
        }
        let fixture = Fixture::new(source, records).await;
        let capture = fixture.read_families(&[EvidenceFamily::OperationalSamples]).await;
        let error = collected_resources::memory_failures(&capture).unwrap_err();
        assert_eq!(
            error.detail,
            Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::UnsupportedSchema)),
            "{shape}"
        );
    }
}
