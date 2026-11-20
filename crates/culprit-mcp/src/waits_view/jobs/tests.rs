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
            TimescaleJobViewKind::ContinuousAggregates,
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
        configuration_check: Some(TimescaleJobProcedure {
            schema: "public".into(),
            name: "private-configuration-check".into(),
        }),
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
            StateSourceCoverage::observed(TIMESCALE_CONTINUOUS_AGGREGATES_SOURCE.into(), 0, true),
        ],
        observations,
    };
    assert!(native.is_valid());
    report.snapshot.operations.jobs =
        Some(Box::new(JobsSnapshot::Timescale { snapshot: Box::new(native), read_complete: true }));
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
        "private-configuration-check",
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
                let JobsSnapshot::Timescale { snapshot: native, .. } =
                    report.snapshot.operations.jobs.as_mut().unwrap().as_mut()
                else {
                    panic!()
                };
                let CollectionTarget::Database(database) = &mut native.context.target else { panic!() };
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

#[test]
fn agent_projection_identifies_original_jobs_and_steps_without_borrowing_current_definitions() {
    let mut report = report();
    report.snapshot.server.engine = Engine::Mssql;
    report.snapshot.identity = None;
    let member = SharedResourceIdentity {
        engine: Engine::Mssql,
        kind: SharedResourceKind::Instance,
        namespace: NativeIdentity::new("PRIVATE-AGENT-MACHINE"),
        native: NativeIdentity::new("PRIVATE-AGENT-SERVER"),
    };
    let job = MssqlAgentJob {
        observed_at: FROM.into(),
        job_id: "aaaaaaaa-1111-2222-3333-bbbbbbbbbbbb".into(),
        name: "current import configuration".into(),
        enabled: true,
        start_step_id: "1".into(),
        version_number: "2".into(),
        created_without_offset: None,
        modified_without_offset: None,
        activity: None,
    };
    let history = MssqlAgentHistory {
        observed_at: UNTIL.into(),
        job_id: job.job_id.clone(),
        instance_id: "2147483647".into(),
        step_id: "1".into(),
        stored_step_name: Some("original failed import".into()),
        run_status: 0,
        run_date: "20261115".into(),
        run_time: "0".into(),
        run_duration: "250101".into(),
        sql_message_id: "0".into(),
        sql_severity: "0".into(),
        retries_attempted: "0".into(),
        reported_server: Some("PRIVATE-AGENT-HISTORY-SERVER".into()),
    };
    let job_id = job.job_id.clone();
    let other_history = MssqlAgentHistory {
        job_id: "cccccccc-1111-2222-3333-dddddddddddd".into(),
        instance_id: "2147483646".into(),
        stored_step_name: Some("original failed cleanup".into()),
        ..history.clone()
    };
    let other_job_id = other_history.job_id.clone();
    let rows = vec![
        OperationalObservation {
            source: MSSQL_AGENT_JOBS_SOURCE.into(),
            key: job.key().unwrap(),
            fact: OperationalFact::MssqlAgentJob(Box::new(job)),
        },
        OperationalObservation {
            source: MSSQL_AGENT_HISTORY_SOURCE.into(),
            key: history.key().unwrap(),
            fact: OperationalFact::MssqlAgentHistory(Box::new(history)),
        },
        OperationalObservation {
            source: MSSQL_AGENT_HISTORY_SOURCE.into(),
            key: other_history.key().unwrap(),
            fact: OperationalFact::MssqlAgentHistory(Box::new(other_history)),
        },
    ];
    let read = MssqlAgentJobsSnapshot {
        context: NativeReadContext {
            target: CollectionTarget::Shared(member.clone()),
            partition: SourcePartition::InstanceLocal {
                member: Some(member),
                epoch: Some("PRIVATE-AGENT-EPOCH".into()),
            },
            observed_from: FROM.into(),
            observed_until: UNTIL.into(),
            database_writable: None,
        },
        metadata: MssqlAgentJobsMetadata {
            engine_edition: 3,
            major_version: 16,
            availability: MssqlAgentAvailability::Supported,
            visibility: MssqlAgentVisibility::Sysadmin,
            history_limit: 256,
            recheck: MssqlAgentMetadataRecheck::Unchanged,
            capabilities: [
                MssqlAgentObject::Jobs,
                MssqlAgentObject::Steps,
                MssqlAgentObject::History,
                MssqlAgentObject::Activity,
                MssqlAgentObject::Sessions,
                MssqlAgentObject::Services,
            ]
            .into_iter()
            .enumerate()
            .map(|(i, object)| MssqlAgentCapability {
                object,
                access: MssqlAgentAccess::Available,
                columns_sha256: Some(digest(object.native_name().as_bytes())),
                object_id: Some((i + 100).to_string()),
            })
            .collect(),
        },
        sources: vec![
            StateSourceCoverage::collected(MSSQL_AGENT_JOBS_SOURCE.into(), 1),
            StateSourceCoverage::collected(MSSQL_AGENT_STEPS_SOURCE.into(), 0),
            StateSourceCoverage::observed(MSSQL_AGENT_HISTORY_SOURCE.into(), 2, false),
            StateSourceCoverage::unavailable(
                MSSQL_AGENT_CONTEXT_SOURCE.into(),
                culprit_core::ErrorCode::PermissionDenied,
                None,
            ),
        ],
        observations: rows,
    };
    assert!(read.is_valid_read(true));
    report.snapshot.operations.jobs =
        Some(Box::new(JobsSnapshot::MssqlAgent { snapshot: Box::new(read), read_complete: true }));
    report.jobs.as_mut().unwrap().summary = "PRIVATE-AGENT-CACHED-PROSE".into();
    let mut output = json!({});
    attach(&mut output, &report);
    assert_eq!(output["jobs"]["native_rows"], 3);
    assert_eq!(output["jobs"]["read_complete"], true);
    assert!(output.to_string().contains("250101"));
    assert!(!output.to_string().contains("PRIVATE-AGENT"));
    let entries = output["jobs"]["entries"].as_array().unwrap();
    let histories = entries.iter().filter(|entry| entry["kind"] == "retained-run").collect::<Vec<_>>();
    assert_eq!(histories.len(), 2);
    assert!(
        histories.iter().any(|entry| entry["job_id"] == job_id && entry.to_string().contains("original failed import"))
    );
    assert!(
        histories
            .iter()
            .any(|entry| entry["job_id"] == other_job_id && entry.to_string().contains("original failed cleanup"))
    );
    assert!(histories.iter().all(|entry| !entry.to_string().contains("current import configuration")));
    assert!(
        entries
            .iter()
            .any(|entry| entry["job_id"] == job_id && entry.to_string().contains("current import configuration"))
    );
    assert!(!output.to_string().contains("2147483647"));
}

#[test]
fn agent_service_states_keep_their_native_labels_and_exclude_unselected_text() {
    let entry = JobEntry {
        source: MSSQL_AGENT_CONTEXT_SOURCE.into(),
        observation_key: "original-agent-context".into(),
        job_id: None,
        kind: culprit_core::JobEntryKind::AgentContext,
        title: "Agent context".into(),
        explanation: "Original service observations".into(),
        metrics: vec![
            Metric::said_text("view.mssql_agent_jobs.field.service_name", "SQL Server Agent (instance A)"),
            Metric::said_text("view.mssql_agent_jobs.field.service_status", "4"),
            Metric::said_text("view.mssql_agent_jobs.field.service_name", "SQL Server (instance A)"),
            Metric::said_text("view.mssql_agent_jobs.field.service_status", "1"),
            Metric::said_text("unselected-command", "PRIVATE-COMMAND-CANARY"),
        ],
        investigation: "Inspect the recorded service state.".into(),
        verify: "Compare a new bounded observation.".into(),
    };
    let output = point(&entry);
    let metrics = output["metrics"].as_array().unwrap();
    assert_eq!(metrics.len(), 4);
    assert!(metrics[0].to_string().contains("SQL Server Agent (instance A)"));
    assert!(metrics[2].to_string().contains("SQL Server (instance A)"));
    assert!(!output.to_string().contains("PRIVATE-COMMAND-CANARY"));
}

#[test]
fn mysql_event_compact_output_uses_exact_original_reference_without_runtime_or_private_body_inference() {
    let unknown = culprit_core::t!("view.mysql_events.unknown");
    for last_started in ["2026-11-19T13:00:00", unknown.as_str()] {
        let entry = JobEntry {
            source: MYSQL_SCHEDULED_EVENTS_SOURCE.into(),
            observation_key: "original-event-catalog-key".into(),
            job_id: None,
            kind: culprit_core::JobEntryKind::State,
            title: culprit_core::t!("view.mysql_events.event.title"),
            explanation: culprit_core::t!("view.mysql_events.event.unknown"),
            metrics: vec![
                Metric::said_text("view.mysql_events.field.name", "nightly_event"),
                Metric::said_text("view.mysql_events.field.schema", "shop"),
                Metric::said_text("view.mysql_events.field.last_started", last_started),
                Metric::said_text("view.mysql_events.field.time_zone", "SYSTEM"),
                Metric::said_text("unselected-event-definition", "PRIVATE-EVENT-BODY-CANARY"),
                Metric::said_text("unselected-event-definer", "PRIVATE-DEFINER-CANARY"),
            ],
            investigation: culprit_core::t!("view.mysql_events.investigate"),
            verify: culprit_core::t!("view.mysql_events.verify"),
        };
        let output = point(&entry);
        assert_eq!(output["kind"], "state");
        assert!(output["job_id"].is_null());
        assert_eq!(output["reference"]["source"], MYSQL_SCHEDULED_EVENTS_SOURCE);
        assert_eq!(output["reference"]["observation_key"], entry.observation_key);
        let metrics = output["metrics"].as_array().unwrap();
        assert_eq!(metrics.len(), 4);
        assert_eq!(metrics[2]["key"], "view.mysql_events.field.last_started");
        assert_eq!(metrics[2]["text"], last_started);
        assert_eq!(metrics[3]["text"], "SYSTEM");
        let text = output.to_string();
        for forbidden in [
            "PRIVATE-EVENT-BODY-CANARY",
            "PRIVATE-DEFINER-CANARY",
            "EVENT_DEFINITION",
            "DEFINER",
            "query_id",
            "native_payload_sha256",
        ] {
            assert!(!text.contains(forbidden));
        }
        for unavailable in ["succeeded", "duration_ms", "current_worker"] {
            assert!(output.get(unavailable).is_none());
        }
        assert!(text.len() <= 4096);
    }
}
