use super::*;
use culprit_collector::analytical::test_support::{
    SuspectPageHistoryCase, assert_suspect_page_history_capture, suspect_page_history_evidence,
};
#[tokio::test]
async fn retained_history_uses_original_proof_and_one_canonical_explanation_without_recovery() {
    for case in SuspectPageHistoryCase::ALL {
        let (source, rows) = suspect_page_history_evidence(case);
        let fixture = Fixture::new(source.clone(), rows.clone()).await;
        let opened = fixture.recovery_object_reads();
        let capture =
            fixture.read_families(&[culprit_collector::analytical::evidence::EvidenceFamily::OperationalSamples]).await;
        assert_suspect_page_history_capture(&capture, &source, &rows);
        let analysis = culprit_engine::collected_resources::suspect_pages(&capture).unwrap();
        assert_eq!(analysis.context.source, source);
        assert_eq!(fixture.recovery_object_reads(), opened);
        let observations = rows
            .iter()
            .map(|row| match row {
                TelemetryRecord::Operational(row) => row,
                _ => unreachable!(),
            })
            .collect::<Vec<_>>();
        let read = SuspectPageHistoryRead::from_source(&source, &observations, true).unwrap();
        let expected = culprit_checks::suspect_pages::explain(&read).unwrap();
        assert_eq!(serde_json::to_value(&*analysis.report).unwrap(), serde_json::to_value(&expected).unwrap());
        assert_eq!(analysis.report.native_source.as_ref().unwrap(), &source.observation.snapshot().unwrap().sources[0]);
        if matches!(case, SuspectPageHistoryCase::Denied | SuspectPageHistoryCase::Unsupported) {
            assert!(analysis.report.retained_rows.is_none());
            assert!(analysis.report.metrics.is_empty());
        } else if case == SuspectPageHistoryCase::RepairedOnly {
            assert_eq!(analysis.report.active_rows.as_deref(), Some("0"));
            assert_eq!(analysis.report.retained_rows.as_deref(), Some("3"));
        }
        assert!(analysis.report.limitations.contains(&culprit_core::t!("view.suspect_pages.limit.incarnation")));
    }
}
#[tokio::test]
async fn absent_original_body_retains_a_gap_and_never_recovers_or_reports_zero() {
    let (source, rows) = suspect_page_history_evidence(SuspectPageHistoryCase::Active);
    let fixture = Fixture::new(source, rows).await;
    let opened = fixture.recovery_object_reads();
    fixture.remove_first_row(TelemetryStream::OperationalSamples);
    let capture =
        fixture.read_families(&[culprit_collector::analytical::evidence::EvidenceFamily::OperationalSamples]).await;
    let analysis = culprit_engine::collected_resources::suspect_pages(&capture).unwrap();
    assert_eq!(analysis.report.source_state, Some(culprit_core::SourceState::Partial));
    assert!(analysis.report.retained_rows.is_none());
    assert!(analysis.report.active_rows.is_none());
    assert_eq!(fixture.recovery_object_reads(), opened);
}
