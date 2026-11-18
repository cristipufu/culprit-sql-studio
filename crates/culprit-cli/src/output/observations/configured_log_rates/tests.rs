use super::*;
use culprit_collector::analytical::test_support::configured_log_rate_evidence_cases;
use culprit_core::telemetry::*;

fn report(case: usize) -> ConfiguredLogRateReport {
    let (source, rows) = configured_log_rate_evidence_cases().remove(case);
    let point = rows.into_iter().find_map(|row| match row {
        TelemetryRecord::Operational(OperationalObservation {
            fact: OperationalFact::ConfiguredLogRateLimits(point),
            ..
        }) => Some(*point),
        _ => None,
    });
    let read = ConfiguredLogRateRead::from_source(&source, point).unwrap();
    culprit_checks::configured_log_rates::explain(&read, true).unwrap()
}

#[test]
fn finite_text_keeps_every_scope_exact_value_unknown_and_qualification() {
    let report = report(0);
    let text = configured_log_rates(&report);
    for entry in &report.entries {
        assert!(text.contains(&entry.title));
        assert!(text.contains(entry.metric.text.as_ref().unwrap()));
    }
    for limitation in &report.limitations {
        assert!(text.contains(limitation));
    }
    assert!(!text.contains("9007199254740992"));
    assert!(!text.contains("fixture.invalid"));
    let json = serde_json::to_value(&report).unwrap();
    assert_eq!(json["entries"][0]["bytes_per_second"], "9007199254740993");
}

#[test]
fn unavailable_or_null_configuration_never_becomes_a_measured_zero_or_current_component() {
    for case in [2, 3] {
        let report = report(case);
        let text = configured_log_rates(&report);
        assert!(text.contains(&report.summary));
        assert!(text.contains(&t!("view.configured_log_rates.limit.components")));
        assert!(!text.contains("0 B/s"));
    }
}
