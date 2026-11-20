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
        assert!(text.contains(&report.native_source.source));
        assert!(text.len() < 16_384);
    }
}

#[test]
fn native_session_and_request_database_context_is_rendered_separately_with_unknowns_and_bounded_escaped_labels() {
    for (case, session_id, request_id) in [
        (SessionCensusCase::Idle, "7", t!("ui.memory_failures.not_reported")),
        (SessionCensusCase::Mars, "4", "7".into()),
        (SessionCensusCase::Dormant, "0", t!("ui.memory_failures.not_reported")),
        (SessionCensusCase::Unknown, "", t!("ui.memory_failures.not_reported")),
    ] {
        let report = report(case);
        let text = session_census(&report);
        let expected_id = if session_id.is_empty() { t!("ui.memory_failures.not_reported") } else { session_id.into() };
        assert!(text.contains(&t!(
            "view.counter_ratios.cli.metric",
            label = t!("view.session_census.field.session_database_id"),
            value = expected_id
        )));
        assert!(text.contains(&t!(
            "view.counter_ratios.cli.metric",
            label = t!("view.session_census.field.request_database_id"),
            value = request_id
        )));
        assert!(report.limitations.contains(&t!("view.session_census.limit.database_ids")));
        assert!(text.contains(&report.limitations[0]));
    }
    let mut report = report(SessionCensusCase::Mars);
    let raw = format!("session\n\u{1b}[31m{}{}", "名".repeat(8), "\u{1b}".repeat(100));
    report.entries[0].fields.iter_mut().find(|field| field.key == "session_database_name").unwrap().value =
        Some(raw.clone());
    let text = session_census(&report);
    let expected = super::super::statement::preview_bounded(&raw, MAX_FIELD_PREVIEW_CHARACTERS);
    assert!(text.contains(&t!(
        "view.counter_ratios.cli.metric",
        label = t!("view.session_census.field.session_database_name"),
        value = &expected
    )));
    assert!(expected.ends_with('…'));
    assert!(expected.chars().count() <= MAX_FIELD_PREVIEW_CHARACTERS + 1);
    assert!(!text.contains('\u{1b}'));
}
