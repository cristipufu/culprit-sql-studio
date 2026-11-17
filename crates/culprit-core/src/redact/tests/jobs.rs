//! Sharing validates the original typed roster before any masking or replacement key can make it look admissible.
use super::*;
use crate::redact::jobs::redact as redact_jobs;
use crate::{DatabaseIdentity, ErrorDetail, MeasurementScope, SourceOutcome, SourceState, telemetry::*};

const PRIVATE: &str = "JOBS-REDACTION-PRIVATE";
const FROM: &str = "2026-11-16T10:00:00.123456Z";
const UNTIL: &str = "2026-11-16T10:00:01.123456Z";

fn procedure(name: &str) -> TimescaleJobProcedure {
    TimescaleJobProcedure { schema: "public".into(), name: format!("/JOBS-REDACTION-PRIVATE/{name}") }
}
fn metadata() -> TimescaleJobsMetadata {
    let views = [
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
    .collect();
    TimescaleJobsMetadata {
        extension: Some(NativeIdentity::new("17000").qualified("database_oid", "16384").qualified("version", "2.31.0")),
        views,
        run_view: Some(TimescaleJobViewKind::History),
        run_visibility: TimescaleJobRunVisibility::JobOwners,
        successful_runs_logged: Some(false),
        database_writable: Some(true),
        run_window: TimescaleJobRunWindow { from: "2026-11-15T10:00:00.123456Z".into(), until: FROM.into() },
    }
}
fn read() -> Box<TimescaleJobsSnapshot> {
    let state = TimescaleJobState {
        position: 1,
        observed_at: FROM.into(),
        job_id: "1000".into(),
        procedure: procedure("current-procedure"),
        owner: "JOBS-REDACTION-PRIVATE-owner".into(),
        scheduled: true,
        schedule_interval: "1 mon".into(),
        max_runtime: "01:00:00".into(),
        max_retries: "-1".into(),
        retry_period: "00:05:00".into(),
        fixed_schedule: Some(true),
        initial_start: Some(TimescaleJobTimestamp::NegativeInfinity),
        next_start: Some(TimescaleJobTimestamp::PositiveInfinity),
        hypertable: Some(procedure("associated-object")),
        statistics: Some(TimescaleJobStatistics {
            reported_job_state: Some(TimescaleReportedJobState::Running),
            last_run_status: Some(TimescaleJobLastRun::Failed),
            last_run_started_at: None,
            last_successful_finish: None,
            last_run_duration: Some("00:00:01.000001".into()),
            next_start: None,
            total_runs: Some("9007199254740993".into()),
            total_successes: Some("0".into()),
            total_failures: Some("1".into()),
        }),
    };
    let run = TimescaleJobRun {
        position: 1,
        observed_at: UNTIL.into(),
        native_run_id: Some("9007199254740994".into()),
        job_id: "1000".into(),
        procedure: Some(TimescaleReportedJobProcedure {
            label: procedure("reported-run-label"),
            origin: TimescaleJobProcedureOrigin::StoredOrCurrent,
        }),
        process_id: Some("73".into()),
        started_at: "2026-11-16T09:00:00.000001Z".into(),
        finished_at: Some("2026-11-16T09:00:01.000002Z".into()),
        succeeded: Some(false),
        sql_state: Some("53200".into()),
    };
    let rows = vec![
        OperationalObservation {
            source: TIMESCALE_JOBS_SOURCE.into(),
            key: state.key().unwrap(),
            fact: OperationalFact::TimescaleJobState(Box::new(state)),
        },
        OperationalObservation {
            source: TIMESCALE_JOB_RUNS_SOURCE.into(),
            key: run.key().unwrap(),
            fact: OperationalFact::TimescaleJobRun(Box::new(run)),
        },
    ];
    let source = CollectionDescription {
        target: DatabaseIdentity::server(
            Engine::Postgres,
            "public-jobs-db",
            "JOBS-REDACTION-PRIVATE-host.invalid",
            5432,
            None,
        )
        .into(),
        partition: SourcePartition::Target,
        source: TIMESCALE_JOBS_SOURCE.into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::Jobs {
            metadata: Box::new(metadata()),
            snapshot: StateSnapshotDescription {
                dataset: SnapshotDataset::Jobs,
                observed_from: FROM.into(),
                observed_until: UNTIL.into(),
                records: rows.len() as u32,
                sources: vec![
                    StateSourceCoverage::collected(TIMESCALE_JOBS_SOURCE.into(), 1),
                    StateSourceCoverage::observed(TIMESCALE_JOB_RUNS_SOURCE.into(), 1, false),
                ],
            },
        },
    };
    let read = TimescaleJobsSnapshot::from_source(&source, rows).unwrap();
    assert!(read.is_valid(), "fixture must be authentic typed state and run evidence");
    Box::new(read)
}
fn snapshot(read: Box<TimescaleJobsSnapshot>) -> Snapshot {
    let mut snapshot: Snapshot =
        serde_json::from_str(include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/postgres-demo.json")))
            .unwrap();
    snapshot.operations.jobs = Some(read);
    snapshot
        .sources
        .retain(|source| source.source != TIMESCALE_JOBS_SOURCE && source.source != TIMESCALE_JOB_RUNS_SOURCE);
    snapshot.sources.push(SourceOutcome {
        source: TIMESCALE_JOBS_SOURCE.into(),
        state: SourceState::Collected,
        reason: Some("JOBS-REDACTION-PRIVATE-prior-source-detail".into()),
        reason_message: None,
    });
    snapshot
}
fn assert_unavailable(snapshot: &Snapshot) -> SourceOutcome {
    assert!(
        snapshot.operations.jobs.is_none(),
        "invalid original rows must be withheld, not repaired or partially published"
    );
    let markers = snapshot.sources.iter().filter(|source| source.source == TIMESCALE_JOBS_SOURCE).collect::<Vec<_>>();
    assert_eq!(markers.len(), 1, "one explicit source gap replaces prior native success");
    let marker = markers[0];
    assert_eq!(marker.state, SourceState::Unavailable);
    assert_eq!(marker.reason_message, Some(crate::message!("view.jobs.invalid")));
    assert_eq!(
        snapshot
            .notes
            .iter()
            .filter(|note| note.collector == TIMESCALE_JOBS_SOURCE
                && note.message_source == Some(crate::message!("view.jobs.invalid")))
            .count(),
        1
    );
    assert!(!serde_json::to_string(snapshot).unwrap().contains(PRIVATE));
    marker.clone()
}
fn assert_rejected_before_masking(original: Box<TimescaleJobsSnapshot>) {
    assert!(!original.is_valid_read(false));
    assert_eq!(
        redact_jobs(original.clone()).unwrap_err().detail,
        Some(ErrorDetail::CollectionStopped(CollectionStop::InvalidPage))
    );
    let snapshot = snapshot(original);
    let before = serde_json::to_value(&snapshot).unwrap();
    let shared = redact_snapshot(&snapshot);
    let marker = assert_unavailable(&shared);
    let shared_again = redact_snapshot(&shared);
    assert_eq!(assert_unavailable(&shared_again), marker, "repeated sharing keeps the same explicit native gap");
    assert_eq!(serde_json::to_value(&snapshot).unwrap(), before, "sharing never rewrites the original native carrier");
}

#[test]
fn valid_state_and_run_facts_stay_valid_exact_and_redaction_stable() {
    let original = read();
    let shared = redact_jobs(original.clone()).unwrap();
    assert!(shared.is_valid());
    assert!(!serde_json::to_string(&shared).unwrap().contains(PRIVATE));
    assert_eq!(shared.sources, original.sources);
    assert_eq!(shared.metadata, original.metadata);
    assert_eq!(shared.context.observed_from, FROM);
    assert_eq!(shared.context.observed_until, UNTIL);
    let OperationalFact::TimescaleJobState(state) = &shared.observations[0].fact else { panic!() };
    assert_eq!(state.statistics.as_ref().unwrap().total_runs.as_deref(), Some("9007199254740993"));
    assert_eq!(state.statistics.as_ref().unwrap().reported_job_state, Some(TimescaleReportedJobState::Running));
    assert_eq!(state.statistics.as_ref().unwrap().last_run_status, Some(TimescaleJobLastRun::Failed));
    assert_eq!(state.initial_start, Some(TimescaleJobTimestamp::NegativeInfinity));
    assert_eq!(state.next_start, Some(TimescaleJobTimestamp::PositiveInfinity));
    assert!(state.owner.starts_with("redacted-"));
    assert_ne!(
        shared.observations[0].key, original.observations[0].key,
        "masked procedure labels require the matching new native row key"
    );
    assert_eq!(shared.observations[0].key, state.key().unwrap());
    let OperationalFact::TimescaleJobRun(run) = &shared.observations[1].fact else { panic!() };
    assert_eq!(run.native_run_id.as_deref(), Some("9007199254740994"));
    assert_eq!(run.sql_state.as_deref(), Some("53200"));
    assert_eq!(run.succeeded, Some(false));
    assert_eq!(run.procedure.as_ref().unwrap().origin, TimescaleJobProcedureOrigin::StoredOrCurrent);
    assert_eq!(shared.observations[1].key, run.key().unwrap());
    assert_eq!(redact_jobs(shared.clone()).unwrap(), shared);
}

#[test]
fn malformed_original_owner_or_contradictory_native_key_cannot_be_repaired_by_masking() {
    let mut missing_owner = read();
    let OperationalFact::TimescaleJobState(state) = &mut missing_owner.observations[0].fact else { panic!() };
    state.owner.clear();
    assert_rejected_before_masking(missing_owner);
    let mut wrong_key = read();
    wrong_key.observations[0].key = digest(b"contradictory-original-key");
    assert_rejected_before_masking(wrong_key);
}

#[test]
fn a_valid_foreign_fact_with_private_identity_and_properties_is_withheld_as_an_explicit_gap() {
    let mut original = read();
    let foreign = OperationalObservation::from_counters(
        TIMESCALE_JOBS_SOURCE,
        NativeCounterSet {
            ratios: Vec::new(),
            ratio_gaps: Vec::new(),
            identity: NativeIdentity::new("JOBS-REDACTION-PRIVATE-foreign-identity")
                .qualified("endpoint", "JOBS-REDACTION-PRIVATE-foreign-host"),
            scope: MeasurementScope::Database,
            counters: vec![NativeCounterSample::gauge(
                "count",
                7,
                CounterUnit::Count,
                MeasurementScope::Database,
                "foreign-native-object",
                "count",
            )],
            properties: [(
                "endpoint".into(),
                "tcp://JOBS-REDACTION-PRIVATE-user:JOBS-REDACTION-PRIVATE-password@foreign.invalid:5432".into(),
            )]
            .into(),
        },
    )
    .unwrap();
    assert!(foreign.is_valid(), "the foreign fact is independently valid, but not admitted job evidence");
    original.observations[0] = foreign;
    assert_rejected_before_masking(original);
}

#[test]
fn genuine_partial_state_and_run_rosters_keep_original_counts_without_becoming_complete() {
    let mut original = read();
    original.sources[1].records += 1;
    assert!(original.is_valid_read(false) && !original.is_valid());
    let shared = redact_jobs(original.clone()).unwrap();
    assert!(shared.is_valid_read(false) && !shared.is_valid());
    assert_eq!(shared.sources, original.sources);
    assert_eq!(shared.observations.len(), original.observations.len());
    assert_eq!(redact_jobs(shared.clone()).unwrap(), shared);
}

#[test]
fn unknown_status_text_is_masked_without_changing_classification_counts_or_source_clocks() {
    let credential = "postgres://reader:JOBS-REDACTION-PRIVATE@private-host.invalid/jobs";
    let path = "/srv/tenants/JOBS-REDACTION-PRIVATE/job-status";
    let mut original = read();
    let OperationalFact::TimescaleJobState(point) = &mut original.observations[0].fact else { panic!() };
    let statistics = point.statistics.as_mut().unwrap();
    statistics.reported_job_state = Some(TimescaleReportedJobState::Unknown { native: credential.into() });
    statistics.last_run_status = Some(TimescaleJobLastRun::Unknown { native: path.into() });
    let original_statistics = statistics.clone();
    assert!(original.is_valid(), "unknown native status values are valid retained context");
    let before = serde_json::to_value(&original).unwrap();
    let shared = redact_jobs(original.clone()).unwrap();
    let OperationalFact::TimescaleJobState(point) = &shared.observations[0].fact else { panic!() };
    let statistics = point.statistics.as_ref().unwrap();
    let Some(TimescaleReportedJobState::Unknown { native: state }) = &statistics.reported_job_state else {
        panic!("redaction must preserve unknown scheduler classification")
    };
    let Some(TimescaleJobLastRun::Unknown { native: status }) = &statistics.last_run_status else {
        panic!("redaction must preserve unknown last-run classification")
    };
    assert!(state.starts_with("redacted-") && status.starts_with("redacted-"));
    assert_ne!(state, credential);
    assert_ne!(status, path);
    let mut expected_statistics = original_statistics;
    expected_statistics.reported_job_state = statistics.reported_job_state.clone();
    expected_statistics.last_run_status = statistics.last_run_status.clone();
    assert_eq!(statistics, &expected_statistics, "all other native statistics stay unchanged");
    assert!(shared.is_valid());
    assert_eq!(shared.metadata, original.metadata);
    assert_eq!(shared.sources, original.sources);
    assert_eq!(shared.context.observed_from, original.context.observed_from);
    assert_eq!(shared.context.observed_until, original.context.observed_until);
    let text = serde_json::to_string(&shared).unwrap();
    for private in [PRIVATE, credential, path] {
        assert!(!text.contains(private), "{private}");
    }
    assert_eq!(redact_jobs(shared.clone()).unwrap(), shared);
    assert_eq!(serde_json::to_value(&original).unwrap(), before, "sharing never rewrites native input");

    let shared_snapshot = redact_snapshot(&snapshot(original));
    let text = serde_json::to_string(&shared_snapshot).unwrap();
    for private in [PRIVATE, credential, path] {
        assert!(!text.contains(private), "{private}");
    }
    assert_eq!(
        serde_json::to_value(redact_snapshot(&shared_snapshot)).unwrap(),
        serde_json::to_value(shared_snapshot).unwrap()
    );
}

#[test]
fn invalid_unknown_status_text_is_rejected_before_masking_could_make_it_valid() {
    for reported_state in [true, false] {
        for native in [String::new(), "x".repeat(129)] {
            let mut original = read();
            let OperationalFact::TimescaleJobState(point) = &mut original.observations[0].fact else { panic!() };
            let statistics = point.statistics.as_mut().unwrap();
            if reported_state {
                statistics.reported_job_state = Some(TimescaleReportedJobState::Unknown { native });
            } else {
                statistics.last_run_status = Some(TimescaleJobLastRun::Unknown { native });
            }
            assert_rejected_before_masking(original);
        }
    }
}
