//! Synthetic installed-view points exercise existing archive protocols, never native work qualification.
use super::*;
use culprit_core::{DatabaseIdentity, Engine, ErrorCode};
#[derive(Clone, Copy, Debug)]
pub enum JobsCase {
    Failed,
    Running,
    Unfinished,
    ErrorsOnly,
    DuplicateState,
    OwnerFilteredEmpty,
    Empty,
    Unavailable,
    ExtensionMissing,
    Limited,
    AggregateConfiguration,
    AggregateNameMatchesCurrentJob,
}
impl JobsCase {
    pub const ALL: &[Self] = &[
        Self::Failed,
        Self::Running,
        Self::Unfinished,
        Self::ErrorsOnly,
        Self::DuplicateState,
        Self::OwnerFilteredEmpty,
        Self::Empty,
        Self::Unavailable,
        Self::ExtensionMissing,
        Self::Limited,
        Self::AggregateConfiguration,
        Self::AggregateNameMatchesCurrentJob,
    ];
}
pub fn jobs_evidence_cases() -> Vec<(CollectionDescription, Vec<TelemetryRecord>)> {
    JobsCase::ALL.iter().copied().map(jobs_evidence).collect()
}
pub fn jobs_evidence(case: JobsCase) -> (CollectionDescription, Vec<TelemetryRecord>) {
    let mut metadata = metadata();
    let mut rows = Vec::new();
    match case {
        JobsCase::Failed | JobsCase::Limited => {
            rows.push(state(1));
            rows.push(run(Some(false), true));
        }
        JobsCase::Running => rows.push(state(1)),
        JobsCase::Unfinished => rows.push(run(None, false)),
        JobsCase::ErrorsOnly => {
            metadata.run_view = Some(TimescaleJobViewKind::Errors);
            metadata.views[2].state = TimescaleJobViewState::Missing;
            metadata.views[2].identity = None;
            let mut point = run(None, true);
            let OperationalFact::TimescaleJobRun(run) = &mut point.fact else { panic!() };
            run.native_run_id = None;
            if let Some(procedure) = &mut run.procedure {
                procedure.origin = TimescaleJobProcedureOrigin::Stored;
            }
            point.key = run.key().unwrap();
            rows.push(point);
        }
        JobsCase::DuplicateState => {
            rows.push(state(1));
            let mut second = state(2);
            let OperationalFact::TimescaleJobState(point) = &mut second.fact else { panic!() };
            point.statistics.as_mut().unwrap().total_runs = Some("9".into());
            rows.push(second);
        }
        JobsCase::AggregateConfiguration => rows.push(aggregate()),
        JobsCase::AggregateNameMatchesCurrentJob => {
            let aggregate = aggregate();
            let OperationalFact::TimescaleContinuousAggregate(point) = &aggregate.fact else { unreachable!() };
            let mut current_job = state(1);
            let OperationalFact::TimescaleJobState(job) = &mut current_job.fact else { unreachable!() };
            job.hypertable =
                Some(TimescaleJobProcedure { schema: point.view.schema.clone(), name: point.view.name.clone() });
            current_job.key = job.key().unwrap();
            rows.extend([aggregate, current_job, run(Some(false), true)]);
        }
        JobsCase::OwnerFilteredEmpty => metadata.run_visibility = TimescaleJobRunVisibility::JobOwners,
        JobsCase::Empty => {}
        JobsCase::Unavailable => {
            metadata.run_view = None;
            for view in &mut metadata.views {
                view.state = TimescaleJobViewState::PermissionDenied;
            }
        }
        JobsCase::ExtensionMissing => {
            metadata.extension = None;
            metadata.run_view = None;
            metadata.run_visibility = TimescaleJobRunVisibility::Unknown;
            metadata.successful_runs_logged = None;
            for view in &mut metadata.views {
                view.state = TimescaleJobViewState::Missing;
                view.identity = None;
            }
        }
    }
    let sources = [TIMESCALE_JOBS_SOURCE, TIMESCALE_JOB_RUNS_SOURCE, TIMESCALE_CONTINUOUS_AGGREGATES_SOURCE]
        .into_iter()
        .map(|source| {
            let count = rows.iter().filter(|row| row.source == source).count() as u32;
            match case {
                JobsCase::Unavailable => {
                    StateSourceCoverage::unavailable(source.into(), ErrorCode::PermissionDenied, None)
                }
                JobsCase::ExtensionMissing => StateSourceCoverage::not_applicable(source.into()),
                JobsCase::Limited => {
                    let mut coverage = StateSourceCoverage::observed(source.into(), count, false);
                    coverage.stopped = Some(CollectionStop::RowLimit);
                    coverage
                }
                _ => StateSourceCoverage::observed(source.into(), count, source != TIMESCALE_JOB_RUNS_SOURCE),
            }
        })
        .collect();
    let source = CollectionDescription {
        target: DatabaseIdentity::server(Engine::Postgres, "jobs-db", "private-jobs-host.invalid", 5432, None).into(),
        partition: SourcePartition::Target,
        source: TIMESCALE_JOBS_SOURCE.into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::Jobs {
            metadata: Box::new(metadata),
            snapshot: StateSnapshotDescription {
                dataset: SnapshotDataset::Jobs,
                observed_from: FROM.into(),
                observed_until: UNTIL.into(),
                records: rows.len() as u32,
                sources,
            },
        },
    };
    assert!(source.matches_request(&CollectionRequest {
        subject: CollectionSubject::Database,
        selection: CollectionSelection::Snapshot { dataset: SnapshotDataset::Jobs },
        budget: CollectionBudget::default()
    }));
    assert!(TimescaleJobsSnapshot::from_source(&source, rows.clone()).unwrap().is_valid());
    (source, rows.into_iter().map(TelemetryRecord::Operational).collect())
}
const FROM: &str = "2026-11-15T12:00:00.123456Z";
const UNTIL: &str = "2026-11-15T12:00:01.123456Z";
fn metadata() -> TimescaleJobsMetadata {
    let views = [
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
                .qualified("view", kind.native_name())
                .qualified("extension_oid", "17000")
                .qualified("columns", digest(kind.native_name().as_bytes())),
        ),
    })
    .collect();
    TimescaleJobsMetadata {
        extension: Some(NativeIdentity::new("17000").qualified("database_oid", "16384").qualified("version", "2.31.0")),
        views,
        run_view: Some(TimescaleJobViewKind::History),
        run_visibility: TimescaleJobRunVisibility::DatabaseOwner,
        successful_runs_logged: Some(false),
        run_window: TimescaleJobRunWindow { from: "2026-11-14T12:00:00.123456Z".into(), until: FROM.into() },
        database_writable: Some(true),
    }
}
fn state(position: u32) -> OperationalObservation {
    let point = TimescaleJobState {
        position,
        observed_at: FROM.into(),
        job_id: "1000".into(),
        procedure: TimescaleJobProcedure { schema: "current_schema".into(), name: "current_procedure".into() },
        owner: "private-current-job-owner".into(),
        scheduled: true,
        schedule_interval: "1 mon".into(),
        max_runtime: "01:00:00".into(),
        max_retries: "-1".into(),
        retry_period: "00:05:00".into(),
        fixed_schedule: Some(true),
        configuration_check: Some(TimescaleJobProcedure {
            schema: "current_schema".into(),
            name: "current_configuration_check".into(),
        }),
        initial_start: Some(TimescaleJobTimestamp::NegativeInfinity),
        next_start: Some(TimescaleJobTimestamp::PositiveInfinity),
        hypertable: None,
        statistics: Some(TimescaleJobStatistics {
            reported_job_state: Some(TimescaleReportedJobState::Running),
            last_run_status: Some(TimescaleJobLastRun::Failed),
            last_run_started_at: None,
            last_successful_finish: None,
            last_run_duration: Some("00:02:00.000001".into()),
            next_start: None,
            total_runs: Some("9007199254740993".into()),
            total_successes: Some("0".into()),
            total_failures: Some("9007199254740993".into()),
        }),
    };
    OperationalObservation {
        source: TIMESCALE_JOBS_SOURCE.into(),
        key: point.key().unwrap(),
        fact: OperationalFact::TimescaleJobState(Box::new(point)),
    }
}
fn run(succeeded: Option<bool>, finished: bool) -> OperationalObservation {
    let point = TimescaleJobRun {
        position: 1,
        observed_at: UNTIL.into(),
        native_run_id: Some("9007199254740993".into()),
        job_id: "1000".into(),
        procedure: Some(TimescaleReportedJobProcedure {
            label: TimescaleJobProcedure { schema: "reported_schema".into(), name: "reported_procedure".into() },
            origin: TimescaleJobProcedureOrigin::StoredOrCurrent,
        }),
        process_id: None,
        started_at: "2026-11-15T11:00:00.000001Z".into(),
        finished_at: finished.then(|| "2026-11-15T11:01:00.000002Z".into()),
        succeeded,
        sql_state: Some("53200".into()),
    };
    OperationalObservation {
        source: TIMESCALE_JOB_RUNS_SOURCE.into(),
        key: point.key().unwrap(),
        fact: OperationalFact::TimescaleJobRun(Box::new(point)),
    }
}
pub fn assert_jobs_capture(
    capture: &crate::analytical::evidence::EvidenceCapture,
    source: &CollectionDescription,
    records: &[TelemetryRecord],
) {
    assert!(capture.is_read_complete());
    assert_eq!(capture.source(), source);
    assert_eq!(capture.records().len(), records.len());
    for actual in capture.records() {
        let expected=records.iter().find(|record|matches!((record,&actual.row().record),(TelemetryRecord::Operational(left),TelemetryRecord::Operational(right)) if left.source==right.source&&left.key==right.key)).unwrap();
        let exported = ExportedRecord::new(source, capture.capture(), expected.clone()).unwrap();
        assert_eq!(actual.row(), &exported);
    }
}

fn aggregate() -> OperationalObservation {
    let relation = |schema: &str, name: &str| TimescaleRelationLabel { schema: schema.into(), name: name.into() };
    let point = TimescaleContinuousAggregate {
        position: 1,
        observed_at: FROM.into(),
        view: relation("public", "hourly readings"),
        source_hypertable: relation("public", "raw readings"),
        materialization_hypertable: relation("_timescaledb_internal", "_materialized_hypertable_2"),
        owner: "native-aggregate-owner".into(),
        materialized_only: false,
        compression_enabled: true,
        granular_refresh_enabled: None,
    };
    OperationalObservation {
        source: TIMESCALE_CONTINUOUS_AGGREGATES_SOURCE.into(),
        key: point.key().unwrap(),
        fact: OperationalFact::TimescaleContinuousAggregate(Box::new(point)),
    }
}
