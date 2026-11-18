use super::*;
use culprit_collector::analytical::test_support::{SessionCensusCase, session_census_evidence};
use culprit_core::telemetry::*;

fn report(case: SessionCensusCase) -> SessionCensusReport {
    let (source, rows) = session_census_evidence(case);
    let observations = rows
        .into_iter()
        .map(|row| match row {
            TelemetryRecord::Activity(row) => *row,
            _ => panic!(),
        })
        .collect();
    culprit_checks::session_census::explain(&ActivityCensusRead::new(source, observations, true).unwrap()).unwrap()
}

#[test]
fn census_text_preserves_exact_values_mars_request_relationships_native_lifetimes_and_nullable_fields() {
    let report = report(SessionCensusCase::Mars);
    let text = session_census(&report);
    assert!(text.contains("9007199254740993"));
    assert!(text.contains("11111111-1111-1111-1111-111111111111"));
    assert!(text.contains("22222222-2222-2222-2222-222222222222"));
    assert!(text.contains("2020-01-01T11:59:54.0000000"));
    assert!(text.contains("2020-01-01T11:59:55.0000000"));
    assert!(text.contains(&t!("ui.memory_failures.not_reported")));
    assert!(text.contains(&report.summary));
    assert_eq!(report.observed_sessions, 1);
    assert_eq!(report.observed_requests, 2);
}

#[test]
fn idle_dormant_unknown_partial_and_unavailable_census_remains_informational_with_original_read_qualification() {
    for case in SessionCensusCase::ALL {
        let report = report(case);
        let text = session_census(&report);
        assert!(text.contains(&report.title));
        assert!(text.contains(&report.summary));
        assert!(text.contains(&report.limitations[0]));
        assert!(text.contains(culprit_core::source_id::SESSIONS));
        assert!(text.len() < 16_384);
    }
}
