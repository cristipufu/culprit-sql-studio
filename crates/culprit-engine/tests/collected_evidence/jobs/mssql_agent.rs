use super::*;
use culprit_collector::analytical::test_support::{MssqlAgentJobsCase, mssql_agent_jobs_evidence};
#[tokio::test]
async fn agent_history_missing_bodies_and_instance_scope_remain_independent_from_current_scan_namespace() {
    let (source, records) = mssql_agent_jobs_evidence(MssqlAgentJobsCase::Failed);
    assert!(matches!(&source.target,CollectionTarget::Shared(target) if target.kind==SharedResourceKind::Instance));
    let fixture = Fixture::new(source.clone(), records.clone()).await;
    let opened = fixture.recovery_object_reads();
    fixture.remove_first_row(TelemetryStream::OperationalSamples);
    let capture = fixture.read_families(FAMILY).await;
    let analysis = collected_resources::jobs(&capture).unwrap();
    assert_eq!(analysis.context.source, source);
    assert_eq!(analysis.report.context.target, source.target);
    assert!(!analysis.report.read_complete);
    assert_eq!(
        (analysis.report.native_rows, analysis.report.observed_rows),
        (records.len() as u32, records.len() as u32 - 1)
    );
    assert!(
        analysis
            .report
            .sources
            .iter()
            .any(|source| source.source == MSSQL_AGENT_CONTEXT_SOURCE && source.state == SourceState::Partial)
    );
    assert!(
        analysis
            .report
            .entries
            .iter()
            .any(|entry| entry.metrics.iter().any(|metric| metric.text.as_deref() == Some("250101")))
    );
    fixture.unpublish();
    let capture = fixture.read_families(FAMILY).await;
    let analysis = collected_resources::jobs(&capture).unwrap();
    assert_eq!(analysis.report.native_rows, records.len() as u32);
    assert_eq!(analysis.report.observed_rows, 0);
    assert!(!analysis.report.read_complete);
    assert!(analysis.report.entries.is_empty());
    assert_eq!(fixture.recovery_object_reads(), opened);
}
#[tokio::test]
async fn denied_agent_and_azure_not_applicable_remain_distinct_even_when_warehouse_bodies_are_empty() {
    for case in [MssqlAgentJobsCase::Denied, MssqlAgentJobsCase::AzureNotApplicable] {
        let (source, rows) = mssql_agent_jobs_evidence(case);
        let fixture = Fixture::new(source.clone(), rows).await;
        let capture = fixture.read_families(FAMILY).await;
        let report = collected_resources::jobs(&capture).unwrap().report;
        assert!(report.entries.is_empty());
        let expected = if matches!(case, MssqlAgentJobsCase::Denied) {
            SourceState::Unavailable
        } else {
            SourceState::NotApplicable
        };
        assert!(report.sources.iter().all(|source| source.state == expected));
        assert_eq!(report.context.target, source.target);
    }
}
