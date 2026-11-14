use super::fixture::Fixture;
use culprit_collector::analytical::{evidence::EvidenceFamily, test_support::native_failure_evidence_cases};
use culprit_core::{Snapshot, SourceOutcome, SourceState, telemetry::*};
use culprit_engine::collected_resources;

fn snapshot(source: &CollectionDescription, records: &[TelemetryRecord]) -> Snapshot {
    let CollectionTarget::Shared(resource) = &source.target else { panic!() };
    let SourceObservation::Snapshot(native) = &source.observation else { panic!() };
    let json = match resource.engine {
        culprit_core::Engine::Mysql => include_str!("../../../../fixtures/mysql-demo.report.json"),
        culprit_core::Engine::Postgres => include_str!("../../../../fixtures/pg-demo.report.json"),
        _ => panic!("native fixture engine"),
    };
    let mut snapshot = serde_json::from_str::<culprit_core::Report>(json).unwrap().snapshot;
    snapshot.operations.native_failures = Some(NativeFailureSnapshot {
        source: native.sources[0].source.clone(),
        observed_from: native.observed_from.clone(),
        observed_until: native.observed_until.clone(),
        resource: Some(resource.clone()),
        observations: records
            .iter()
            .map(|record| {
                let TelemetryRecord::Operational(observation) = record else { panic!() };
                match &observation.fact {
                    OperationalFact::MysqlErrorLog(row) => NativeFailureRecord::MysqlErrorLog(row.as_ref().clone()),
                    OperationalFact::PostgresLog(row) => NativeFailureRecord::PostgresLog(row.clone()),
                    _ => panic!("native fixture fact"),
                }
            })
            .collect(),
    });
    snapshot.sources.retain(|source| source.source != native.sources[0].source);
    snapshot.sources.push(SourceOutcome {
        source: native.sources[0].source.clone(),
        state: native.sources[0].state,
        reason: None,
        reason_message: None,
    });
    snapshot
}

#[tokio::test]
async fn shared_native_errors_reuse_scan_diagnoses_without_provider_credentials_or_query_owner_joins() {
    for (source, records) in native_failure_evidence_cases() {
        let expected = culprit_checks::analyze(snapshot(&source, &records)).native_failures.unwrap();
        let fixture = Fixture::new(source.clone(), records.clone()).await;
        let capture = fixture.read_families(&[EvidenceFamily::OperationalSamples]).await;
        assert!(capture.is_read_complete(), "{:?}", capture.gaps());
        let result = collected_resources::native_failures(&capture).unwrap();
        assert_eq!(serde_json::to_value(&result.report).unwrap(), serde_json::to_value(expected).unwrap());
        assert_eq!(result.context.source, source);
        assert!(result.context.source.target.database().is_none());
        assert_eq!(result.context.recovery_manifest_sha256, fixture.range.manifest_digest());
        assert_eq!(result.context.records.len(), records.len());
        assert_eq!(
            result.report.entries.iter().map(|entry| entry.position).collect::<Vec<_>>(),
            (0..records.len() as u32).collect::<Vec<_>>()
        );
        for row in records {
            let canonical = ExportedRecord::new(&source, capture.capture(), row).unwrap();
            assert!(result.context.records.iter().any(|actual| actual.row == canonical));
        }
        let serialized_report = serde_json::to_string(&result.report).unwrap();
        assert!(!serialized_report.contains("statement\":"), "original native rows have a single output owner");
    }
}

#[tokio::test]
async fn store_row_loss_is_partial_without_rewriting_the_native_sources_history() {
    for (source, records) in native_failure_evidence_cases() {
        let SourceObservation::Snapshot(native) = &source.observation else { panic!() };
        let expected_native = native.clone();
        let fixture = Fixture::new(source, records.clone()).await;
        fixture.remove_first_row(TelemetryStream::OperationalSamples);
        let capture = fixture.read_families(&[EvidenceFamily::OperationalSamples]).await;
        let result = collected_resources::native_failures(&capture).unwrap();
        assert_eq!(result.report.source_state, Some(SourceState::Partial));
        assert!(result.report.source_reason.is_some());
        assert_eq!(result.report.entries.len(), records.len() - 1);
        assert_eq!(result.report.entries[0].position, 1);
        assert!(!result.context.read_gaps.is_empty());
        let SourceObservation::Snapshot(actual_native) = &result.context.source.observation else { panic!() };
        assert_eq!(actual_native, &expected_native);
    }
}

#[tokio::test]
async fn an_unpublished_capture_keeps_source_and_store_unknowns_visible_without_invented_events() {
    for (source, records) in native_failure_evidence_cases() {
        let fixture = Fixture::new(source, records).await;
        fixture.unpublish();
        let capture = fixture.read_families(&[EvidenceFamily::OperationalSamples]).await;
        let result = collected_resources::native_failures(&capture).unwrap();
        assert!(result.report.entries.is_empty());
        assert_eq!(result.report.source_state, Some(SourceState::Partial));
        assert!(result.report.source_reason.is_some());
        assert!(result.context.records.is_empty());
        assert_eq!(
            result.context.read_gaps[0].reason,
            culprit_core::collected_analysis::EvidenceGapReason::NotPublished
        );
    }
}

#[tokio::test]
async fn wrong_dataset_or_resource_kind_cannot_acquire_native_error_semantics() {
    for (mut source, records) in native_failure_evidence_cases() {
        let CollectionTarget::Shared(resource) = &mut source.target else { panic!() };
        resource.kind = SharedResourceKind::Pool;
        let fixture = Fixture::new(source.clone(), records.clone()).await;
        let capture = fixture.read_families(&[EvidenceFamily::OperationalSamples]).await;
        assert_eq!(
            collected_resources::native_failures(&capture).unwrap_err().detail,
            Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::UnsupportedSchema))
        );
        let CollectionTarget::Shared(resource) = &mut source.target else { panic!() };
        resource.kind = SharedResourceKind::Instance;
        let SourceObservation::Snapshot(native) = &mut source.observation else { panic!() };
        native.dataset = SnapshotDataset::Operational;
        let fixture = Fixture::new(source, records).await;
        let capture = fixture.read_families(&[EvidenceFamily::OperationalSamples]).await;
        assert_eq!(
            collected_resources::native_failures(&capture).unwrap_err().detail,
            Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::UnsupportedSchema))
        );
    }
}
