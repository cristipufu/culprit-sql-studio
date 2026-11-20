use super::*;

fn with_state(check: Option<TimescaleJobProcedure>) -> TimescaleJobsSnapshot {
    let mut snapshot = input();
    let point = TimescaleJobState {
        position: 1,
        observed_at: snapshot.context.observed_from.clone(),
        job_id: "1000".into(),
        procedure: TimescaleJobProcedure { schema: "public".into(), name: "executed_procedure".into() },
        configuration_check: check,
        owner: "current_owner".into(),
        scheduled: true,
        schedule_interval: "01:00:00".into(),
        max_runtime: "00:10:00".into(),
        max_retries: "-1".into(),
        retry_period: "00:05:00".into(),
        fixed_schedule: Some(true),
        initial_start: None,
        next_start: None,
        hypertable: None,
        statistics: None,
    };
    snapshot.observations.push(OperationalObservation {
        source: TIMESCALE_JOBS_SOURCE.into(),
        key: point.key().unwrap(),
        fact: OperationalFact::TimescaleJobState(Box::new(point)),
    });
    snapshot.sources[0] = StateSourceCoverage::collected(TIMESCALE_JOBS_SOURCE.into(), 1);
    assert!(snapshot.is_valid());
    snapshot
}
fn check() -> TimescaleJobProcedure {
    TimescaleJobProcedure { schema: "checks".into(), name: "validate_configuration".into() }
}

#[test]
fn current_configuration_validation_is_named_separately_from_the_executing_procedure_and_past_failure() {
    let snapshot = with_state(Some(check()));
    let original = snapshot.clone();
    let report = explain_native(&snapshot, true).unwrap();
    let state = report.entries.iter().find(|entry| entry.kind == JobEntryKind::State).unwrap();
    let run = report.entries.iter().find(|entry| entry.kind == JobEntryKind::RetainedRun).unwrap();
    assert!(state.metrics.iter().any(|metric| metric.is("view.jobs.metric.configuration_check")
        && metric.text.as_deref() == Some("checks.validate_configuration")));
    assert!(
        state.metrics.iter().any(|metric| metric.is("view.jobs.metric.procedure")
            && metric.text.as_deref() == Some("public.executed_procedure"))
    );
    assert!(state.explanation.contains(&t!("view.jobs.configuration_check.recorded")));
    assert!(run.metrics.iter().all(|metric| !metric.is("view.jobs.metric.configuration_check")));
    assert!(!run.explanation.contains(&t!("view.jobs.configuration_check.recorded")));
    assert_eq!(snapshot, original);
}

#[test]
fn an_unknown_optional_check_preserves_existing_state_words_without_an_invented_validation_result() {
    let snapshot = with_state(None);
    let report = explain_native(&snapshot, true).unwrap();
    let state = report.entries.iter().find(|entry| entry.kind == JobEntryKind::State).unwrap();
    assert_eq!(state.explanation, format!("{} {}", t!("view.jobs.state.unknown"), t!("view.jobs.state.no_statistics")));
    assert!(state.metrics.iter().all(|metric| !metric.is("view.jobs.metric.configuration_check")));
    assert!(!state.explanation.contains(&t!("view.jobs.configuration_check.recorded")));
}

#[test]
fn validation_registration_does_not_change_success_unfinished_or_failed_native_outcomes() {
    for (succeeded, finished, title) in [
        (Some(true), true, "view.jobs.run.success_title"),
        (None, false, "view.jobs.run.unverified_title"),
        (Some(false), true, "view.jobs.run.failed_title"),
    ] {
        let mut snapshot = with_state(Some(check()));
        let OperationalFact::TimescaleJobRun(run) = &mut snapshot.observations[0].fact else { panic!() };
        run.succeeded = succeeded;
        if !finished {
            run.finished_at = None;
        }
        let report = explain_native(&snapshot, true).unwrap();
        let run = report.entries.iter().find(|entry| entry.kind == JobEntryKind::RetainedRun).unwrap();
        assert_eq!(run.title, culprit_core::i18n::t(title, &[("job", "1000".into())]));
        assert!(run.metrics.iter().all(|metric| !metric.is("view.jobs.metric.configuration_check")));
    }
}

#[test]
fn partial_statistics_and_read_coverage_retain_the_recorded_check_but_invalid_native_ownership_is_refused() {
    let mut snapshot = with_state(Some(check()));
    snapshot.sources[0].state = SourceState::Partial;
    snapshot.sources[0].complete_scope = false;
    snapshot.metadata.views.iter_mut().find(|view| view.kind == TimescaleJobViewKind::Statistics).unwrap().state =
        TimescaleJobViewState::PermissionDenied;
    let report = explain_native(&snapshot, false).unwrap();
    assert!(report.limitations.contains(&t!("view.jobs.limit.statistics")));
    assert!(report.limitations.contains(&t!("view.jobs.limit.read")));
    assert!(
        report
            .entries
            .iter()
            .any(|entry| entry.metrics.iter().any(|metric| metric.is("view.jobs.metric.configuration_check")))
    );
    for change in ["clock", "view", "source"] {
        let mut invalid = snapshot.clone();
        match change {
            "clock" => {
                let OperationalFact::TimescaleJobState(point) = &mut invalid.observations[1].fact else { panic!() };
                point.observed_at = "2021-01-01T00:00:00Z".into();
            }
            "view" => {
                invalid.metadata.views.iter_mut().find(|view| view.kind == TimescaleJobViewKind::Jobs).unwrap().state =
                    TimescaleJobViewState::PermissionDenied
            }
            _ => invalid.observations[1].source = "another-source".into(),
        }
        assert!(explain_native(&invalid, false).is_err(), "{change}");
    }
}
