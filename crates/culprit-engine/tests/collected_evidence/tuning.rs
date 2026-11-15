use super::*;
use culprit_collector::analytical::{
    evidence::{EvidenceFamily, RecoveryPayloadBudget},
    test_support::{self, TuningCase},
};
use culprit_core::SourceState;
use culprit_engine::collected_resources;

#[tokio::test]
async fn authenticated_tuning_points_reuse_the_pure_view_without_inventing_query_or_management_authority() {
    for (_, source, records) in test_support::tuning_evidence_cases() {
        let fixture = Fixture::new(source.clone(), records.clone()).await;
        let capture = fixture.read_families(&[EvidenceFamily::TuningObservations]).await;
        let analysis = collected_resources::tuning(&capture).unwrap();
        assert_eq!(analysis.context.records.len(), records.len());
        let rows = capture
            .records()
            .iter()
            .map(|record| match &record.row().record {
                TelemetryRecord::Tuning(observation) => observation,
                _ => panic!("tuning record"),
            })
            .collect::<Vec<_>>();
        let pure = culprit_checks::tuning::explain(&rows, &analysis.report.sources).unwrap();
        assert_eq!(serde_json::to_value(&pure).unwrap(), serde_json::to_value(&analysis.report).unwrap());
        assert_eq!(analysis.report.entries.len(), records.len());
        assert!(analysis.context.read_gaps.is_empty());
        let derived = serde_json::to_string(&analysis.report).unwrap();
        assert!(!derived.contains("EXEC ") && !derived.contains("OPTION(MAXDOP"));
        for entry in &analysis.report.entries {
            assert!(rows.iter().any(|row| row.source == entry.source && row.fact.key() == entry.observation_key));
        }
    }
}

#[tokio::test]
async fn missing_warehouse_rows_do_not_erase_native_failure_or_scope_context() {
    let (source, records) = test_support::tuning_evidence(TuningCase::Complete);
    let fixture = Fixture::new(source, records).await;
    fixture.remove_first_row(TelemetryStream::TuningObservations);
    let capture = fixture.read_families(&[EvidenceFamily::TuningObservations]).await;
    let analysis = collected_resources::tuning(&capture).unwrap();
    assert_eq!(analysis.report.entries.len(), 3);
    assert!(!analysis.context.read_gaps.is_empty());
    assert!(analysis.report.sources.iter().all(|source| source.state == SourceState::Partial));
    assert!(
        analysis
            .report
            .entries
            .iter()
            .any(|entry| entry.scope == Some(culprit_core::QueryStoreScope::ReplicaGroup { id: "7".into() }))
    );
}

#[tokio::test]
async fn large_tuning_bodies_require_explicit_bounded_recovery_and_never_become_commands() {
    let (source, records) = test_support::tuning_evidence(TuningCase::LargeDefinition);
    let fixture = Fixture::new(source.clone(), records.clone()).await;
    let before = fixture.recovery_object_reads();
    let preview = fixture.read_families(&[EvidenceFamily::TuningObservations]).await;
    assert!(!preview.is_read_complete());
    assert_eq!(fixture.recovery_object_reads(), before);
    let partial = collected_resources::tuning(&preview).unwrap();
    assert_eq!(partial.report.entries.len(), records.len() - 1);
    let full = fixture
        .read_families_with_recovery(&[EvidenceFamily::TuningObservations], RecoveryPayloadBudget::default())
        .await;
    test_support::assert_tuning_capture(&full, &source, &records);
    let complete = collected_resources::tuning(&full).unwrap();
    assert_eq!(complete.report.entries.len(), records.len());
    assert_eq!(full.metrics().recovery_objects, 1);
    assert_eq!(fixture.recovery_object_reads(), before + 1);
    assert!(!serde_json::to_string(&complete.report).unwrap().contains("column_399"));
}

#[tokio::test]
async fn wrong_dataset_family_or_shared_subject_cannot_be_reinterpreted_as_tuning() {
    let (source, records) = test_support::tuning_evidence(TuningCase::Complete);
    let fixture = Fixture::new(source, records).await;
    let wrong = fixture.read_families(&[EvidenceFamily::OperationalSamples]).await;
    assert!(collected_resources::tuning(&wrong).is_err());
    let (source, records) = test_support::object_definition_evidence_cases().remove(0);
    let fixture = Fixture::new(source, records).await;
    let wrong = fixture.read_families(&[EvidenceFamily::ObjectDefinitions]).await;
    assert!(collected_resources::tuning(&wrong).is_err());
    let (mut source, records) = test_support::tuning_evidence(TuningCase::Complete);
    let (shared, _) = test_support::shared_resource(SharedResourceKind::Instance, "native-instance");
    source.target = shared.target;
    let fixture = Fixture::new(source, records).await;
    let wrong = fixture.read_families(&[EvidenceFamily::TuningObservations]).await;
    assert!(collected_resources::tuning(&wrong).is_err());
}
