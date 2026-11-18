use super::fixture::Fixture;
use culprit_collector::analytical::{
    evidence::{EvidenceFamily, EvidenceGapReason},
    test_support::{self, SessionCensusCase},
};
use culprit_core::{SourceState, telemetry::*};

#[tokio::test]
async fn original_idle_dormant_mars_unknown_and_source_gaps_reuse_the_same_census_without_a_native_query() {
    for case in SessionCensusCase::ALL {
        let (source, rows) = test_support::session_census_evidence(case);
        let fixture = Fixture::new(source.clone(), rows.clone()).await;
        let opened = fixture.recovery_object_reads();
        let capture = fixture.read_families(&[EvidenceFamily::ActivityObservations]).await;
        test_support::assert_activity_capture(&capture, &source, &rows);
        let analysis = culprit_engine::collected_resources::session_census(&capture).unwrap();
        let original = ActivityCensusRead::new(
            source.clone(),
            rows.into_iter()
                .map(|row| match row {
                    TelemetryRecord::Activity(row) => *row,
                    _ => panic!(),
                })
                .collect(),
            true,
        )
        .unwrap();
        let expected = culprit_checks::session_census::explain(&original).unwrap();
        assert_eq!(analysis.context.source, source);
        assert_eq!(analysis.report.context, expected.context);
        assert_eq!(analysis.report.observed_at, expected.observed_at);
        assert_eq!(analysis.report.scope, expected.scope);
        assert_eq!(analysis.report.native_source, expected.native_source);
        assert_eq!(analysis.report.observed_sessions, expected.observed_sessions);
        assert_eq!(analysis.report.observed_requests, expected.observed_requests);
        assert_eq!(analysis.report.entries.len(), expected.entries.len());
        for (actual, expected) in analysis.report.entries.iter().zip(expected.entries) {
            assert_eq!(actual.activity, expected.activity);
            assert_eq!(actual.fields, expected.fields);
            assert!(actual.reference.as_ref().unwrap().is_valid());
        }
        assert_eq!(fixture.recovery_object_reads(), opened);
    }
}

#[tokio::test]
async fn missing_mars_request_body_preserves_original_native_population_and_does_not_hydrate_or_sum_session_totals() {
    let (source, rows) = test_support::session_census_evidence(SessionCensusCase::Mars);
    let fixture = Fixture::new(source.clone(), rows).await;
    fixture.remove_first_row(TelemetryStream::ActivityObservations);
    let opened = fixture.recovery_object_reads();
    let capture = fixture.read_families(&[EvidenceFamily::ActivityObservations]).await;
    assert!(!capture.is_read_complete());
    assert!(capture.gaps().iter().any(|gap| gap.reason == EvidenceGapReason::MissingRows));
    let analysis = culprit_engine::collected_resources::session_census(&capture).unwrap();
    assert_eq!(analysis.context.source, source);
    assert_eq!(analysis.report.native_rows, 2);
    assert_eq!(analysis.report.native_source.records, 2);
    assert_eq!(analysis.report.native_source.state, SourceState::Collected);
    assert_eq!(analysis.report.sources[0].state, SourceState::Partial);
    assert_eq!((analysis.report.observed_sessions, analysis.report.observed_requests), (1, 1));
    assert_eq!(analysis.report.entries.len(), 1);
    assert!(!analysis.report.read_complete);
    assert_eq!(fixture.recovery_object_reads(), opened);
}
