use super::*;
fn explain_native(snapshot: &TimescaleJobsSnapshot, read_complete: bool) -> Result<JobsReport> {
    explain(&JobsSnapshot::Timescale { snapshot: Box::new(snapshot.clone()), read_complete })
}
fn input() -> TimescaleJobsSnapshot {
    let context = NativeReadContext {
        target: culprit_core::DatabaseIdentity::server(
            culprit_core::Engine::Postgres,
            "db",
            "private-host",
            5432,
            None,
        )
        .into(),
        partition: SourcePartition::Target,
        observed_from: "2020-01-02T12:00:00Z".into(),
        observed_until: "2020-01-02T12:00:01Z".into(),
        database_writable: Some(true),
    };
    let metadata = TimescaleJobsMetadata {
        extension: Some(NativeIdentity::new("17000").qualified("database_oid", "16384").qualified("version", "2.31.0")),
        views: [
            TimescaleJobViewKind::Jobs,
            TimescaleJobViewKind::Statistics,
            TimescaleJobViewKind::History,
            TimescaleJobViewKind::Errors,
        ]
        .into_iter()
        .enumerate()
        .map(|(index, kind)| TimescaleJobViewCapability {
            kind,
            state: TimescaleJobViewState::Available,
            identity: Some(
                NativeIdentity::new((18000 + index).to_string())
                    .qualified("extension_oid", "17000")
                    .qualified("view", kind.native_name())
                    .qualified("columns", digest(kind.native_name().as_bytes())),
            ),
        })
        .collect(),
        run_view: Some(TimescaleJobViewKind::History),
        run_visibility: TimescaleJobRunVisibility::JobOwners,
        successful_runs_logged: Some(false),
        run_window: TimescaleJobRunWindow { from: "2020-01-01T12:00:00Z".into(), until: context.observed_from.clone() },
        database_writable: Some(true),
    };
    let point = TimescaleJobRun {
        position: 1,
        observed_at: context.observed_until.clone(),
        native_run_id: Some("9007199254740993".into()),
        job_id: "1000".into(),
        procedure: Some(TimescaleReportedJobProcedure {
            label: TimescaleJobProcedure { schema: "native".into(), name: "reported".into() },
            origin: TimescaleJobProcedureOrigin::StoredOrCurrent,
        }),
        process_id: None,
        started_at: "2020-01-02T11:00:00Z".into(),
        finished_at: Some("2020-01-02T11:01:00Z".into()),
        succeeded: Some(false),
        sql_state: Some("53200".into()),
    };
    let observation = OperationalObservation {
        source: TIMESCALE_JOB_RUNS_SOURCE.into(),
        key: point.key().unwrap(),
        fact: OperationalFact::TimescaleJobRun(Box::new(point)),
    };
    TimescaleJobsSnapshot {
        context,
        metadata,
        sources: vec![
            StateSourceCoverage::observed(TIMESCALE_JOBS_SOURCE.into(), 0, true),
            StateSourceCoverage::observed(TIMESCALE_JOB_RUNS_SOURCE.into(), 1, false),
        ],
        observations: vec![observation],
    }
}
#[test]
fn finished_native_failure_reuses_sqlstate_without_current_worker_pressure_or_historical_definition_join() {
    let input = input();
    let original = serde_json::to_value(&input).unwrap();
    let report = explain_native(&input, true).unwrap();
    let entry = &report.entries[0];
    assert_eq!(entry.observation_key, input.observations[0].key);
    assert!(entry.explanation.contains(&t!("view.native_failures.cause.allocation.explanation")));
    assert!(entry.metrics.iter().any(|metric| metric.text.as_deref() == Some("9007199254740993")));
    assert!(entry.explanation.contains(&t!("view.jobs.limit.labels")));
    assert!(!serde_json::to_string(&report).unwrap().contains("ActivityKey"));
    assert_eq!(serde_json::to_value(&input).unwrap(), original);
}
#[test]
fn null_outcome_or_unfinished_row_and_error_view_membership_do_not_become_failure_or_crash() {
    for missing in ["outcome", "finish", "errors"] {
        let mut input = input();
        let row = &mut input.observations[0];
        let OperationalFact::TimescaleJobRun(point) = &mut row.fact else { panic!() };
        match missing {
            "outcome" => point.succeeded = None,
            "finish" => point.finished_at = None,
            _ => {
                point.succeeded = None;
                point.native_run_id = None;
                if let Some(procedure) = &mut point.procedure {
                    procedure.origin = TimescaleJobProcedureOrigin::Stored;
                }
                input.metadata.run_view = Some(TimescaleJobViewKind::Errors);
            }
        }
        row.key = point.key().unwrap();
        let report = explain_native(&input, true).unwrap();
        assert_eq!(report.entries[0].title, t!("view.jobs.run.unverified_title", job = "1000"));
        assert!(!report.entries[0].explanation.contains(&t!("view.native_failures.cause.allocation.explanation")));
    }
}
#[test]
fn missing_archive_rows_and_permission_outcomes_keep_original_counts_and_explicit_read_gaps() {
    let mut input = input();
    input.observations.clear();
    assert!(explain_native(&input, true).is_err());
    let report = explain_native(&input, false).unwrap();
    assert_eq!((report.native_rows, report.observed_rows), (1, 0));
    assert!(!report.read_complete);
    assert!(report.entries.is_empty());
    assert_eq!(report.sources[1].state, SourceState::Partial);
    input.sources[1] = StateSourceCoverage::unavailable(
        TIMESCALE_JOB_RUNS_SOURCE.into(),
        culprit_core::ErrorCode::PermissionDenied,
        None,
    );
    assert_eq!(explain_native(&input, true).unwrap().sources[1].state, SourceState::Unavailable);
}
#[test]
fn presentation_limits_preserve_raw_counts_and_do_not_hide_a_late_recorded_failure() {
    let mut input = input();
    let original = input.observations[0].clone();
    let mut rows = Vec::new();
    for i in 0..MAX_JOB_ENTRIES + 10 {
        let mut row = original.clone();
        let OperationalFact::TimescaleJobRun(point) = &mut row.fact else { panic!() };
        point.position = i as u32 + 1;
        point.succeeded = Some(true);
        row.key = point.key().unwrap();
        rows.push(row);
    }
    rows.push(original.clone());
    let last = rows.last_mut().unwrap();
    let OperationalFact::TimescaleJobRun(point) = &mut last.fact else { panic!() };
    point.position = MAX_JOB_ENTRIES as u32 + 11;
    last.key = point.key().unwrap();
    let failure = rows.last().unwrap().key.clone();
    input.sources[1].records = rows.len() as u32;
    input.observations = rows;
    let report = explain_native(&input, true).unwrap();
    assert!(report.entries_limited);
    assert_eq!(report.entries.len(), MAX_JOB_ENTRIES);
    assert_eq!(report.entries[0].observation_key, failure);
    assert_eq!(report.observed_rows as usize, MAX_JOB_ENTRIES + 11);
}

#[test]
fn a_stored_reported_label_is_available_without_becoming_a_definition_or_resource_owner_binding() {
    let mut input = input();
    input.metadata.run_view = Some(TimescaleJobViewKind::Errors);
    let row = &mut input.observations[0];
    let OperationalFact::TimescaleJobRun(point) = &mut row.fact else { panic!() };
    point.succeeded = None;
    point.native_run_id = None;
    point.procedure.as_mut().unwrap().origin = TimescaleJobProcedureOrigin::Stored;
    row.key = point.key().unwrap();
    let report = explain_native(&input, true).unwrap();
    assert!(report.entries[0].explanation.contains(&t!("view.jobs.limit.stored_label")));
    assert_eq!(report.entries[0].title, t!("view.jobs.run.unverified_title", job = "1000"));
    assert!(!report.entries[0].metrics.iter().any(|metric| metric.is("view.jobs.metric.owner")));
}

mod policy;

mod agent;

mod configuration_check;
