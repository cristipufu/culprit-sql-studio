use super::*;
use culprit_core::{QueryWaitCoverage, WaitStat};

fn capture(recorded_ms: f64) -> Snapshot {
    let mut snapshot = crate::waits::snapshot_with(Engine::Mssql);
    let mut query = crate::support::tests::sample();
    query.text = "SELECT id, payload FROM application_events WHERE id > @id".into();
    query.executions = 2.0;
    query.total_time_ms = 10_000.0;
    query.mean_time_ms = 5_000.0;
    query.rows = Some(4_000.0);
    query.waits = vec![WaitStat {
        category: WaitClass::Network.category().into(),
        wait_type: Some("Native.TransportWait".into()),
        wait_ms: recorded_ms,
        tasks: None,
    }];
    snapshot.taken_at = "2026-11-13T00:00:00Z".into();
    let workload = snapshot.workload.as_mut().unwrap();
    workload.queries = vec![query];
    workload.total_time_ms = 10_000.0;
    workload.total_executions = 2.0;
    workload.window_hours = 24.0;
    workload.window_start = Some("2026-11-12T00:00:00Z".into());
    workload.window_end = Some(snapshot.taken_at.clone());
    workload.timeline = None;
    snapshot
}

#[test]
fn parallel_network_totals_are_not_elapsed_shares_or_proof_of_a_slow_client() {
    let snapshot = capture(40_000.0);
    let findings = network_wait(&snapshot);
    assert_eq!(findings.len(), 1);
    let finding = &findings[0];
    let recorded = finding.metrics.iter().find(|metric| metric.is("check.network_wait.metric.recorded")).unwrap();
    assert_eq!(recorded.value, 40_000.0);
    assert_eq!(finding.impact.db_time_share, None);
    assert_eq!(finding.impact.time_ms_per_day, None);
    assert!(finding.metrics.iter().all(|metric| !matches!(metric.unit, Unit::Ratio)));
    assert!(finding.title.contains(&format::duration(40_000.0)) && !finding.title.contains('%'));
    assert!(finding.title.contains("2.0K counted rows per execution"), "{}", finding.title);
    assert!(finding.summary.contains("does not by itself identify a slow client"), "{}", finding.summary);
    assert!(!finding.summary.contains("had its results ready"), "{}", finding.summary);
    let fix = &finding.fix.as_ref().unwrap().summary;
    assert!(fix.contains("Inspect the native wait"), "{fix}");
    assert!(fix.contains("only when that evidence identifies the bottleneck"), "{fix}");
    assert!(finding.verify[0].contains("equivalent useful work, parameters and load"));
}

#[test]
fn positive_network_classes_do_not_depend_on_an_engine_name_or_full_capture() {
    for engine in Engine::ALL {
        for complete in [false, true] {
            let mut snapshot = capture(40_000.0);
            snapshot.server.engine = engine;
            snapshot.workload.as_mut().unwrap().queries[0].waits_coverage =
                Some(QueryWaitCoverage { classes: vec![WaitClass::Network], complete });
            let cases = network_waits(&snapshot);
            assert_eq!(cases.len(), 1);
            assert_eq!(cases[0].recorded_ms, 40_000.0);
            assert_eq!(cases[0].complete, complete);
        }
    }
    let mut snapshot = capture(40_000.0);
    let query = &mut snapshot.workload.as_mut().unwrap().queries[0];
    query.waits.clear();
    query.waits_coverage = Some(QueryWaitCoverage { classes: vec![WaitClass::Network], complete: false });
    assert!(network_waits(&snapshot).is_empty(), "an unobserved class does not become a measured zero");
}

#[test]
fn invalid_rows_or_execution_counts_do_not_invent_result_size_context() {
    for invalid in [f64::NAN, f64::INFINITY, -1.0] {
        let mut snapshot = capture(40_000.0);
        snapshot.workload.as_mut().unwrap().queries[0].rows = Some(invalid);
        assert!(network_waits(&snapshot)[0].rows_per_execution.is_none());
    }
    for invalid in [0.0, -1.0, f64::NAN, f64::INFINITY] {
        let mut snapshot = capture(40_000.0);
        snapshot.workload.as_mut().unwrap().queries[0].executions = invalid;
        assert!(network_waits(&snapshot)[0].rows_per_execution.is_none());
    }
}

#[test]
fn invalid_and_overflowing_network_totals_are_unknown() {
    for invalid in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -1.0, 0.0] {
        assert!(network_waits(&capture(invalid)).is_empty(), "{invalid}");
    }
    let mut snapshot = capture(f64::MAX);
    let query = &mut snapshot.workload.as_mut().unwrap().queries[0];
    query.waits.push(query.waits[0].clone());
    assert!(network_waits(&snapshot).is_empty());
}

#[test]
fn unknown_or_short_native_period_does_not_turn_a_large_network_total_into_a_high_rate() {
    for hours in [0.0, 0.5] {
        let mut snapshot = capture(3_600_000.0);
        let workload = snapshot.workload.as_mut().unwrap();
        workload.window_start = None;
        workload.window_end = None;
        workload.window_hours = hours;
        assert_eq!(network_waits(&snapshot)[0].severity, Severity::Low);
    }
}

#[test]
fn counted_rows_do_not_establish_that_a_write_sent_a_large_result() {
    let mut snapshot = capture(40_000.0);
    snapshot.workload.as_mut().unwrap().queries[0].text =
        "UPDATE application_events SET archived = 1 WHERE created_at < @cutoff".into();
    let findings = network_wait(&snapshot);
    assert_eq!(findings.len(), 1);
    assert!(findings[0].title.contains("counted rows"));
    assert!(!findings[0].title.contains("sending") && !findings[0].title.contains("returning"));
}
