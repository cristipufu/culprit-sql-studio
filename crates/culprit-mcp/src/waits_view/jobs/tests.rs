use super::*;
use culprit_core::{DatabaseIdentity, Engine, SourceOutcome, SourceState, telemetry::*};
const FROM: &str = "2026-11-15T12:00:00.123456Z";
const UNTIL: &str = "2026-11-15T12:00:01.123456Z";
fn metadata() -> TimescaleJobsMetadata {
    TimescaleJobsMetadata {
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
        database_writable: Some(true),
        run_window: TimescaleJobRunWindow { from: "2026-11-14T12:00:00.123456Z".into(), until: FROM.into() },
    }
}
fn state(index: u32) -> OperationalObservation {
    let point = TimescaleJobState {
        position: index + 1,
        observed_at: FROM.into(),
        job_id: (1000 + index).to_string(),
        procedure: TimescaleJobProcedure { schema: "public".into(), name: "private-job-procedure".into() },
        owner: "private-job-owner".into(),
        scheduled: true,
        schedule_interval: "1 mon".into(),
        max_runtime: "01:00:00".into(),
        max_retries: "-1".into(),
        retry_period: "00:05:00".into(),
        fixed_schedule: Some(true),
        initial_start: None,
        next_start: None,
        hypertable: None,
        statistics: Some(TimescaleJobStatistics {
            reported_job_state: Some(TimescaleReportedJobState::Running),
            last_run_status: None,
            last_run_started_at: None,
            last_successful_finish: None,
            last_run_duration: None,
            next_start: None,
            total_runs: Some("9007199254740993".into()),
            total_successes: Some("0".into()),
            total_failures: None,
        }),
    };
    OperationalObservation {
        source: TIMESCALE_JOBS_SOURCE.into(),
        key: point.key().unwrap(),
        fact: OperationalFact::TimescaleJobState(Box::new(point)),
    }
}
fn report() -> Box<Report> {
    let mut report: Box<Report> =
        serde_json::from_str(include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/pg-demo.report.json")))
            .unwrap();
    let database =
        DatabaseIdentity::server(Engine::Postgres, "private-job-database", "private-job-host.invalid", 5432, None);
    report.snapshot.server.engine = database.engine;
    report.snapshot.server.database = database.database.clone();
    report.snapshot.bind_identity(database.clone());
    let mut observations = (0..8).map(state).collect::<Vec<_>>();
    let run = TimescaleJobRun {
        position: 1,
        observed_at: UNTIL.into(),
        native_run_id: Some("9007199254740994".into()),
        job_id: "1000".into(),
        procedure: Some(TimescaleReportedJobProcedure {
            label: TimescaleJobProcedure { schema: "public".into(), name: "private-job-reported-procedure".into() },
            origin: TimescaleJobProcedureOrigin::StoredOrCurrent,
        }),
        process_id: Some("2147483647".into()),
        started_at: "2026-11-15T11:00:00Z".into(),
        finished_at: Some("2026-11-15T11:01:00Z".into()),
        succeeded: Some(false),
        sql_state: Some("53200".into()),
    };
    observations.push(OperationalObservation {
        source: TIMESCALE_JOB_RUNS_SOURCE.into(),
        key: run.key().unwrap(),
        fact: OperationalFact::TimescaleJobRun(Box::new(run)),
    });
    let native = TimescaleJobsSnapshot {
        context: NativeReadContext {
            target: database.into(),
            partition: SourcePartition::Target,
            observed_from: FROM.into(),
            observed_until: UNTIL.into(),
            database_writable: Some(true),
        },
        metadata: metadata(),
        sources: vec![
            StateSourceCoverage::collected(TIMESCALE_JOBS_SOURCE.into(), 8),
            StateSourceCoverage::observed(TIMESCALE_JOB_RUNS_SOURCE.into(), 1, false),
        ],
        observations,
    };
    assert!(native.is_valid());
    report.snapshot.operations.jobs = Some(Box::new(native));
    report
        .snapshot
        .sources
        .retain(|source| !matches!(source.source.as_str(), TIMESCALE_JOBS_SOURCE | TIMESCALE_JOB_RUNS_SOURCE));
    report.snapshot.sources.push(SourceOutcome {
        source: TIMESCALE_JOBS_SOURCE.into(),
        state: SourceState::Collected,
        reason: None,
        reason_message: None,
    });
    report.jobs = culprit_engine::explain_jobs(&report.snapshot).unwrap().map(Box::new);
    report
}
#[test]
fn jobs_recompute_raw_points_keep_full_counts_and_bound_presentation_without_physical_context() {
    let report = report();
    let mut output = json!({});
    attach(&mut output, &report);
    assert_eq!(output["jobs"]["entries"].as_array().unwrap().len(), ENTRY_LIMIT);
    assert_eq!(output["jobs"]["entries_presented"], ENTRY_LIMIT);
    assert_eq!(output["jobs"]["entries_retained"], 9);
    assert_eq!(output["jobs"]["entries_limited"], true);
    assert_eq!(output["jobs"]["native_rows"], 9);
    assert_eq!(output["jobs"]["observed_rows"], 9);
    assert_eq!(output["jobs"]["read_complete"], true);
    let text = output.to_string();
    assert!(text.contains("9007199254740993"));
    assert!(text.contains(FROM));
    for private in [
        "private-job-database",
        "private-job-host",
        "private-job-owner",
        "private-job-procedure",
        "private-job-reported-procedure",
        "2147483647",
    ] {
        assert!(!text.contains(private), "{private}");
    }
}
#[test]
fn stale_or_missing_derived_reports_never_replace_the_original_native_jobs() {
    let mut report = report();
    let mut expected = json!({});
    attach(&mut expected, &report);
    report.jobs.as_mut().unwrap().title = "STALE-PRIVATE-TITLE".into();
    report.jobs.as_mut().unwrap().summary = "STALE-PRIVATE-SUMMARY".into();
    report.jobs.as_mut().unwrap().native_rows = 999;
    report.jobs.as_mut().unwrap().entries[0]
        .metrics
        .push(Metric::said_text("view.jobs.metric.total_runs", "STALE-PRIVATE-COUNT"));
    let mut actual = json!({});
    attach(&mut actual, &report);
    assert_eq!(actual, expected);
    report.jobs = None;
    let mut actual = json!({});
    attach(&mut actual, &report);
    assert_eq!(actual, expected);
}
#[test]
fn valid_foreign_carriers_are_refused_even_when_a_cached_report_claims_matching_jobs() {
    for changed in ["database", "engine", "endpoint", "incarnation"] {
        let mut report = report();
        match changed {
            "database" => report.snapshot.server.database = "different-database".into(),
            "engine" => report.snapshot.server.engine = Engine::Mysql,
            "endpoint" => report.snapshot.bind_identity(DatabaseIdentity::server(
                Engine::Postgres,
                "private-job-database",
                "different.invalid",
                5432,
                None,
            )),
            _ => {
                let CollectionTarget::Database(database) =
                    &mut report.snapshot.operations.jobs.as_mut().unwrap().context.target
                else {
                    panic!()
                };
                database.incarnation = Some("captured-native-db".into());
                report.snapshot.identity.as_mut().unwrap().incarnation = Some("other-native-db".into());
            }
        }
        assert!(report.snapshot.operations.jobs.as_ref().unwrap().is_valid());
        assert!(report.jobs.is_some(), "a stale cached view must not admit foreign facts");
        let mut output = json!({});
        attach(&mut output, &report);
        assert!(output.get("jobs").is_none(), "{changed}");
        assert_eq!(output["jobs_unavailable"]["reason"], culprit_core::t!("view.jobs.invalid"));
        assert!(!output["jobs_unavailable"].to_string().contains("native_rows"));
    }
}
#[test]
fn unknown_outer_identity_retains_independent_native_points_with_the_shared_namespace_limit() {
    let mut report = report();
    report.snapshot.identity = None;
    let mut output = json!({});
    attach(&mut output, &report);
    assert_eq!(output["jobs"]["native_rows"], 9);
    assert!(
        output["jobs"]["limitations"]
            .as_array()
            .unwrap()
            .contains(&json!(culprit_core::t!("view.observations.limit.namespace")))
    );
}
#[test]
fn source_only_failure_remains_explicit_and_a_stale_cached_view_cannot_resurrect_missing_raw_points() {
    let mut report = report();
    report.snapshot.operations.jobs = None;
    report
        .snapshot
        .sources
        .retain(|source| !matches!(source.source.as_str(), TIMESCALE_JOBS_SOURCE | TIMESCALE_JOB_RUNS_SOURCE));
    report.snapshot.sources.push(SourceOutcome {
        source: TIMESCALE_JOBS_SOURCE.into(),
        state: SourceState::Unavailable,
        reason: Some("Native permission denied.".into()),
        reason_message: None,
    });
    assert!(report.jobs.is_some());
    let mut output = json!({});
    attach(&mut output, &report);
    assert_eq!(output["jobs_unavailable"]["sources"][0]["state"], "unavailable");
    assert_eq!(output["jobs_unavailable"]["sources"][0]["reason"], "Native permission denied.");
    assert!(output.get("jobs").is_none());
    assert!(!output["jobs_unavailable"].to_string().contains("native_rows"));
}
