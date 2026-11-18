use super::fixture::Fixture;
use culprit_collector::analytical::{
    evidence::{EvidenceFamily, EvidenceGapReason},
    test_support::{JobsCase, assert_jobs_capture, jobs_evidence, jobs_evidence_cases},
};
use culprit_core::{SourceState, telemetry::*};
use culprit_engine::collected_resources;
const FAMILY: &[EvidenceFamily] = &[EvidenceFamily::OperationalSamples];
#[tokio::test]
async fn original_job_capabilities_outcomes_counts_and_receipts_reuse_the_same_pure_view_without_recovery() {
    for (source, records) in jobs_evidence_cases()
        .into_iter()
        .chain(culprit_collector::analytical::test_support::mssql_agent_jobs_evidence_cases())
    {
        let rows = records
            .iter()
            .map(|record| {
                let TelemetryRecord::Operational(row) = record else { panic!() };
                row.clone()
            })
            .collect();
        let snapshot = JobsSnapshot::from_source(&source, rows, true).unwrap();
        let expected = culprit_checks::jobs::explain(&snapshot).unwrap();
        let fixture = Fixture::new(source.clone(), records.clone()).await;
        let opened = fixture.recovery_object_reads();
        let capture = fixture.read_families(FAMILY).await;
        assert_jobs_capture(&capture, &source, &records);
        let analysis = collected_resources::jobs(&capture).unwrap();
        assert_eq!(serde_json::to_value(&analysis.report).unwrap(), serde_json::to_value(expected).unwrap());
        assert_eq!(analysis.context.source, source);
        assert_eq!(analysis.context.records.len(), records.len());
        assert_eq!(analysis.report.native_rows as usize, records.len());
        assert!(analysis.report.read_complete);
        assert_eq!(fixture.recovery_object_reads(), opened);
        assert_eq!(capture.metrics().recovery_objects, 0);
        for entry in &analysis.report.entries {
            assert!(records.iter().any(|record|matches!(record,TelemetryRecord::Operational(row) if row.source==entry.source&&row.key==entry.observation_key)));
        }
    }
}
#[tokio::test]
async fn missing_archive_rows_do_not_become_healthy_empty_history_and_original_native_counts_survive() {
    let (source, records) = jobs_evidence(JobsCase::Failed);
    let fixture = Fixture::new(source.clone(), records.clone()).await;
    let opened = fixture.recovery_object_reads();
    fixture.remove_first_row(TelemetryStream::OperationalSamples);
    let capture = fixture.read_families(FAMILY).await;
    let analysis = collected_resources::jobs(&capture).unwrap();
    assert!(!analysis.report.read_complete);
    assert_eq!(analysis.context.source, source);
    assert_eq!(analysis.report.native_rows as usize, records.len());
    assert_eq!(analysis.report.observed_rows as usize, records.len() - 1);
    assert!(analysis.context.read_gaps.iter().any(|gap| gap.reason == EvidenceGapReason::MissingRows));
    assert!(analysis.report.sources.iter().all(|source| source.state == SourceState::Partial));
    fixture.unpublish();
    let capture = fixture.read_families(FAMILY).await;
    let analysis = collected_resources::jobs(&capture).unwrap();
    assert!(!analysis.report.read_complete);
    assert_eq!(analysis.report.observed_rows, 0);
    assert_eq!(analysis.report.native_rows as usize, records.len());
    assert!(analysis.report.entries.is_empty());
    assert!(analysis.context.read_gaps.iter().any(|gap| gap.reason == EvidenceGapReason::NotPublished));
    assert_eq!(fixture.recovery_object_reads(), opened);
}
#[tokio::test]
async fn native_nulls_and_duplicate_job_ids_never_gain_worker_current_definition_or_aggregate_proof() {
    for case in [
        JobsCase::Unfinished,
        JobsCase::ErrorsOnly,
        JobsCase::DuplicateState,
        JobsCase::OwnerFilteredEmpty,
        JobsCase::Unavailable,
    ] {
        let (source, records) = jobs_evidence(case);
        let fixture = Fixture::new(source, records.clone()).await;
        let capture = fixture.read_families(FAMILY).await;
        let analysis = collected_resources::jobs(&capture).unwrap();
        if matches!(case, JobsCase::Unfinished | JobsCase::ErrorsOnly) {
            assert!(
                analysis
                    .report
                    .entries
                    .iter()
                    .all(|entry| entry.title == culprit_core::t!("view.jobs.run.unverified_title", job = "1000"))
            );
            assert!(
                analysis
                    .report
                    .entries
                    .iter()
                    .flat_map(|entry| &entry.metrics)
                    .all(|metric| !metric.is("view.jobs.metric.owner") && !metric.is("view.jobs.metric.procedure"))
            );
        }
        if matches!(case, JobsCase::DuplicateState) {
            assert_eq!(analysis.report.entries.len(), 2);
            let text = serde_json::to_string(&analysis.report).unwrap();
            assert!(text.contains("9007199254740993"));
            assert!(
                analysis
                    .report
                    .entries
                    .iter()
                    .any(|entry| entry.metrics.iter().any(|metric| metric.text.as_deref() == Some("9")))
            );
        }
        let wrong = fixture.read_families(&[EvidenceFamily::ActivityObservations]).await;
        assert!(collected_resources::jobs(&wrong).is_err());
    }
}

mod mssql_agent;
