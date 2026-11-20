use super::*;
use culprit_collector::analytical::{
    evidence::{EvidenceFamily, RecoveryPayloadBudget},
    test_support::{self, StatementGrantCase},
};
use culprit_core::{SourceState, collected_resource_analysis::CollectedStatementGrantAnalysis};
use culprit_engine::collected_resources;

const FAMILY: &[EvidenceFamily] = &[EvidenceFamily::OperationalSamples];

fn original_wrapper(capture: &culprit_collector::analytical::evidence::EvidenceCapture) -> StatementGrantSnapshot {
    StatementGrantSnapshot {
        capture: capture.capture().clone(),
        description: capture.source().clone(),
        observations: capture
            .records()
            .iter()
            .map(|record| {
                let TelemetryRecord::Operational(row) = &record.row().record else { panic!() };
                let OperationalFact::MssqlStatementGrants(grant) = &row.fact else { panic!() };
                grant.as_ref().clone()
            })
            .collect(),
    }
}

fn native_rows_are_referenced_once(analysis: &CollectedStatementGrantAnalysis, original: &StatementGrantSnapshot) {
    for entry in &analysis.report.entries {
        assert_eq!(entry.snapshot_key, original.key().unwrap());
        assert!(original.observations.iter().any(|row| row.context.identity == entry.identity
            && row.context.identity.key().unwrap() == entry.observation_key));
    }
    assert_eq!(analysis.context.source, original.description);
    assert_eq!(analysis.context.capture, original.capture);
}

#[tokio::test]
async fn archived_native_entries_reuse_the_scan_view_with_exact_capture_context_and_no_workload_join() {
    for (_, source, records) in test_support::statement_grant_evidence_cases() {
        let fixture = Fixture::new(source.clone(), records.clone()).await;
        let before = fixture.recovery_object_reads();
        let capture = fixture.read_families(FAMILY).await;
        test_support::assert_statement_grant_capture(&capture, &source, &records);
        let analysis = collected_resources::statement_grants(&capture).unwrap();
        let original = original_wrapper(&capture);
        let pure = culprit_checks::statement_grants::explain(std::slice::from_ref(&original), &analysis.report.sources)
            .unwrap();
        assert_eq!(serde_json::to_value(&pure).unwrap(), serde_json::to_value(&analysis.report).unwrap());
        assert_eq!(analysis.context.records.len(), records.len());
        assert_eq!(analysis.report.entries.len(), records.len());
        assert!(analysis.context.read_gaps.is_empty());
        native_rows_are_referenced_once(&analysis, &original);
        assert_eq!(fixture.recovery_object_reads(), before);
        let report = serde_json::to_value(&analysis.report).unwrap();
        assert!(report.get("workload").is_none() && report.get("findings").is_none());
        assert!(!serde_json::to_string(&report).unwrap().contains("MAX_GRANT_PERCENT"));
    }
}

#[tokio::test]
async fn a_missing_warehouse_row_retains_other_entries_and_the_unchanged_native_header() {
    let (source, records) = test_support::statement_grant_evidence(StatementGrantCase::Complete);
    let fixture = Fixture::new(source.clone(), records).await;
    fixture.remove_first_row(TelemetryStream::OperationalSamples);
    let capture = fixture.read_families(FAMILY).await;
    assert!(!capture.is_read_complete());
    let analysis = collected_resources::statement_grants(&capture).unwrap();
    assert_eq!(analysis.report.entries.len(), 1);
    assert_eq!(analysis.report.sources[0].state, SourceState::Partial);
    assert_eq!(analysis.report.reads[0].coverage[0].state, SourceState::Collected);
    assert_eq!(analysis.report.reads[0].coverage[0].records, 2);
    assert!(!analysis.report.reads[0].read_complete);
    assert_eq!(analysis.context.source, source);
    assert!(!analysis.context.read_gaps.is_empty());
    assert_eq!(
        analysis.report.entries[0]
            .metrics
            .iter()
            .find(|metric| metric.is("check.memory_grant.metric.runs"))
            .unwrap()
            .text
            .as_deref(),
        Some("9007199254740993")
    );
    native_rows_are_referenced_once(&analysis, &original_wrapper(&capture));
}

#[tokio::test]
async fn a_preview_gap_requires_opt_in_whole_batch_recovery_without_inventing_native_or_current_authority() {
    let (source, records) = test_support::statement_grant_evidence(StatementGrantCase::LargeStatement);
    let fixture = Fixture::new(source.clone(), records.clone()).await;
    let before = fixture.recovery_object_reads();
    let capture = fixture.read_families(FAMILY).await;
    let preview = collected_resources::statement_grants(&capture).unwrap();
    assert_eq!(preview.report.entries.len(), 1);
    assert_eq!(fixture.recovery_object_reads(), before);
    let full = fixture.read_families_with_recovery(FAMILY, RecoveryPayloadBudget::default()).await;
    test_support::assert_statement_grant_capture(&full, &source, &records);
    let complete = collected_resources::statement_grants(&full).unwrap();
    assert_eq!(complete.report.entries.len(), 2);
    assert!(complete.report.reads[0].read_complete);
    assert_eq!(complete.context.source, source);
    assert_eq!(full.metrics().recovery_objects, 1);
    assert_eq!(fixture.recovery_object_reads(), before + 1);
    native_rows_are_referenced_once(&complete, &original_wrapper(&full));
}

#[tokio::test]
async fn an_unpublished_capture_produces_an_explicit_incomplete_section_without_claiming_empty_memory_demand() {
    let (source, records) = test_support::statement_grant_evidence(StatementGrantCase::Complete);
    let fixture = Fixture::new(source.clone(), records).await;
    fixture.unpublish();
    let capture = fixture.read_families(FAMILY).await;
    let analysis = collected_resources::statement_grants(&capture).unwrap();
    assert!(analysis.report.entries.is_empty());
    assert_eq!(analysis.context.source, source);
    assert!(!analysis.context.read_gaps.is_empty());
    assert_eq!(analysis.report.sources[0].state, SourceState::Partial);
    assert!(!analysis.report.reads[0].read_complete);
}

#[tokio::test]
async fn wrong_dataset_family_subject_partition_or_native_fact_is_not_cached_grant_evidence() {
    let (source, records) = test_support::statement_grant_evidence(StatementGrantCase::Complete);
    let fixture = Fixture::new(source, records).await;
    let wrong = fixture.read_families(&[EvidenceFamily::TuningObservations]).await;
    assert_eq!(
        collected_resources::statement_grants(&wrong).unwrap_err().detail,
        Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::UnsupportedSchema)),
    );
    for change in 0..4 {
        let (mut source, mut records) = test_support::statement_grant_evidence(StatementGrantCase::Complete);
        match change {
            0 => {
                let SourceObservation::Snapshot(description) = &mut source.observation else { panic!() };
                description.dataset = SnapshotDataset::StatementCounters;
            }
            1 => {
                let (shared, _) = test_support::shared_resource(SharedResourceKind::Instance, "native-instance");
                let CollectionTarget::Shared(mut resource) = shared.target else { panic!() };
                // This is a well-formed source partition with the wrong analytical subject. A foreign-engine
                // target is rejected earlier at native export, covered separately below.
                resource.engine = source.target.engine();
                source.target = CollectionTarget::Shared(resource);
            }
            2 => source.partition = SourcePartition::Target,
            _ => {
                let TelemetryRecord::Operational(row) = &mut records[0] else { panic!() };
                // Suspect-page facts now require their dedicated original source. A generic native counter
                // remains valid raw evidence here, so this still exercises the SDK's wrong-kind refusal.
                row.fact = OperationalFact::Counter(NativeCounterSample::gauge(
                    "unrelated-native-counter",
                    1,
                    CounterUnit::Count,
                    culprit_core::MeasurementScope::Database,
                    "unrelated_native_view",
                    "native_value",
                ));
            }
        }
        let fixture = Fixture::new(source, records).await;
        let wrong = fixture.read_families(FAMILY).await;
        let expected = if change <= 1 { DeliveryProblem::UnsupportedSchema } else { DeliveryProblem::Integrity };
        assert_eq!(
            collected_resources::statement_grants(&wrong).unwrap_err().detail,
            Some(culprit_core::ErrorDetail::Delivery(expected)),
            "case {change}",
        );
    }
}

#[test]
fn a_foreign_engine_target_cannot_be_exported_under_the_original_native_cache_member() {
    let (mut source, records) = test_support::statement_grant_evidence(StatementGrantCase::Complete);
    let capture = CaptureIdentity { observation_id: "original-grant-read".into(), revision: None };
    assert!(ExportedRecord::new(&source, &capture, records[0].clone()).is_ok());
    let (shared, _) = test_support::shared_resource(SharedResourceKind::Instance, "native-instance");
    assert_ne!(shared.target.engine(), source.target.engine());
    source.target = shared.target;
    assert!(!source.partition_is_consistent());
    let error = ExportedRecord::new(&source, &capture, records[0].clone()).unwrap_err();
    assert_eq!(error.detail, Some(culprit_core::ErrorDetail::CollectionStopped(CollectionStop::InvalidPage)));
}
