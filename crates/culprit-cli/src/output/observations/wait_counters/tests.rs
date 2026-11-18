use super::*;
use culprit_collector::analytical::test_support::{WaitCounterCase, wait_counter_evidence};
use culprit_core::telemetry::*;

fn report(case: WaitCounterCase) -> WaitCounterReport {
    let (source, rows) = wait_counter_evidence(case);
    let rows = rows
        .into_iter()
        .map(|row| {
            let TelemetryRecord::WaitCounter(row) = row else { panic!() };
            row
        })
        .collect();
    let read = WaitCounterSnapshot::from_source(&source, rows, true).unwrap();
    culprit_checks::wait_counters::explain(&read, source.target.engine()).unwrap()
}

#[test]
fn bounded_wait_text_keeps_exact_original_fields_clocks_and_all_qualifications() {
    let report = report(WaitCounterCase::Exact);
    let text = wait_counters(&report);
    assert!(text.contains("9007199254740993"));
    assert!(!text.contains("9007199254740992"));
    assert!(text.contains("2020-01-01T12:00:00.000000001Z"));
    assert!(text.contains("2020-01-01T12:00:00.000000009Z"));
    for limitation in &report.limitations {
        assert!(text.contains(limitation));
    }
    assert!(report.entries.len() > super::super::super::BRIEF_FINDING_LIMIT);
    assert!(!text.contains(&report.entries.last().unwrap().title));
    assert!(text.contains(&t!("view.wait_counters.limit.presentation")));
}

#[test]
fn unavailable_and_empty_are_distinct_and_inconsistent_signal_never_becomes_resource_zero() {
    let unavailable = report(WaitCounterCase::Denied);
    let empty = report(WaitCounterCase::Empty);
    assert_ne!(wait_counters(&unavailable), wait_counters(&empty));
    assert_eq!(unavailable.sources[0].state, culprit_core::SourceState::Unavailable);
    let inconsistent = report(WaitCounterCase::InconsistentSignal);
    assert_eq!(inconsistent.entries[0].resource_wait_time_ms, None);
    let text = wait_counters(&inconsistent);
    assert!(text.contains(&t!("view.wait_counters.issue.signal_exceeds_total")));
    assert!(text.contains(&t!("view.wait_counters.unknown")));
}
