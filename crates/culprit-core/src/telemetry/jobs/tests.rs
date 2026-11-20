use super::*;
use crate::{Engine, ErrorCode, SourceState};
mod continuous_aggregates;

fn metadata() -> TimescaleJobsMetadata {
    let extension = NativeIdentity::new("17000").qualified("database_oid", "16384").qualified("version", "2.31.0");
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
                .qualified("extension_oid", "17000")
                .qualified("view", kind.native_name())
                .qualified("columns", digest(kind.native_name().as_bytes())),
        ),
    })
    .collect();
    TimescaleJobsMetadata {
        extension: Some(extension),
        views,
        run_view: Some(TimescaleJobViewKind::History),
        run_visibility: TimescaleJobRunVisibility::DatabaseOwner,
        successful_runs_logged: Some(true),
        run_window: TimescaleJobRunWindow { from: "2020-01-01T12:00:00Z".into(), until: "2020-01-02T12:00:00Z".into() },
        database_writable: Some(true),
    }
}
fn run() -> TimescaleJobRun {
    TimescaleJobRun {
        position: 1,
        observed_at: "2020-01-02T12:00:00.5Z".into(),
        native_run_id: Some("9007199254740993".into()),
        job_id: "1000".into(),
        procedure: None,
        process_id: None,
        started_at: "2020-01-02T11:00:00Z".into(),
        finished_at: None,
        succeeded: None,
        sql_state: Some("53200".into()),
    }
}
fn source(rows: &[OperationalObservation]) -> CollectionDescription {
    CollectionDescription {
        target: DatabaseIdentity::server(Engine::Postgres, "owned-db", "owned.invalid", 5432, None).into(),
        partition: SourcePartition::Target,
        source: TIMESCALE_JOBS_SOURCE.into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::Jobs {
            metadata: Box::new(metadata()),
            snapshot: StateSnapshotDescription {
                dataset: SnapshotDataset::Jobs,
                observed_from: "2020-01-02T12:00:00Z".into(),
                observed_until: "2020-01-02T12:00:01Z".into(),
                records: rows.len() as u32,
                sources: vec![
                    StateSourceCoverage::observed(TIMESCALE_JOBS_SOURCE.into(), 0, true),
                    StateSourceCoverage::observed(TIMESCALE_JOB_RUNS_SOURCE.into(), rows.len() as u32, false),
                    StateSourceCoverage::observed(TIMESCALE_CONTINUOUS_AGGREGATES_SOURCE.into(), 0, true),
                ],
            },
        },
    }
}
fn row(run: TimescaleJobRun) -> OperationalObservation {
    OperationalObservation {
        source: TIMESCALE_JOB_RUNS_SOURCE.into(),
        key: run.key().unwrap(),
        fact: OperationalFact::TimescaleJobRun(Box::new(run)),
    }
}
#[test]
fn unfinished_runs_and_null_pids_never_become_native_failure_or_worker_proof() {
    let original = run();
    assert!(original.is_valid());
    assert_eq!(original.failure_kind(), None);
    let mut completed = original.clone();
    completed.finished_at = Some("2020-01-02T11:01:00Z".into());
    completed.succeeded = Some(false);
    assert_eq!(completed.failure_kind(), Some(PostgresFailureKind::AllocationFailure));
    completed.succeeded = Some(true);
    assert_eq!(completed.failure_kind(), None);
    let row = row(original);
    let snapshot = TimescaleJobsSnapshot::from_source(&source(std::slice::from_ref(&row)), vec![row]).unwrap();
    assert!(snapshot.is_valid());
    assert!(!snapshot.context.has_qualified_local_owner());
    assert_eq!(
        serde_json::from_slice::<TimescaleJobsSnapshot>(&serde_json::to_vec(&snapshot).unwrap()).unwrap(),
        snapshot
    );
}
#[test]
fn exact_original_view_window_and_source_are_required_at_export_even_with_no_rows() {
    let row = row(run());
    let original = source(std::slice::from_ref(&row));
    assert!(
        ExportedRecord::new(
            &original,
            &CaptureIdentity { observation_id: "jobs-fixture".into(), revision: None },
            TelemetryRecord::Operational(row.clone())
        )
        .is_ok()
    );
    for defect in ["clock", "window", "extension", "view", "partition", "source", "bare"] {
        let mut changed = original.clone();
        match defect {
            "clock" => {
                let SourceObservation::Jobs { snapshot, .. } = &mut changed.observation else { panic!() };
                snapshot.observed_until = "2020-01-02T12:00:00.1Z".into();
            }
            "window" => {
                let SourceObservation::Jobs { metadata, .. } = &mut changed.observation else { panic!() };
                metadata.run_window.until = "2020-01-02T10:00:00Z".into();
            }
            "extension" => {
                let SourceObservation::Jobs { metadata, .. } = &mut changed.observation else { panic!() };
                metadata.extension.as_mut().unwrap().id = "17001".into();
            }
            "view" => {
                let SourceObservation::Jobs { metadata, .. } = &mut changed.observation else { panic!() };
                metadata.views[2].state = TimescaleJobViewState::PermissionDenied;
            }
            "partition" => changed.partition = SourcePartition::InstanceLocal { member: None, epoch: None },
            "source" => changed.source = "another-jobs-source".into(),
            _ => changed.observation = SourceObservation::Snapshot(changed.observation.snapshot().unwrap().clone()),
        }
        assert!(
            ExportedRecord::new(
                &changed,
                &CaptureIdentity { observation_id: "jobs-fixture".into(), revision: None },
                TelemetryRecord::Operational(row.clone())
            )
            .is_err(),
            "{defect}"
        );
        if defect != "clock" {
            assert!(!changed.observation.covers_request() || !changed.partition_is_consistent(), "{defect}");
        }
    }
}
#[test]
fn missing_recovered_rows_stay_partial_and_duplicate_job_ids_are_not_merged() {
    let first = row(run());
    let mut second = run();
    second.position = 2;
    let second = row(second);
    let source = source(&[first.clone(), second.clone()]);
    let complete = TimescaleJobsSnapshot::from_source(&source, vec![first.clone(), second]).unwrap();
    assert!(complete.is_valid());
    let partial = TimescaleJobsSnapshot::from_source(&source, vec![first.clone()]).unwrap();
    assert!(partial.is_valid_read(false));
    assert!(!partial.is_valid());
    assert!(TimescaleJobsSnapshot::from_source(&source, vec![first.clone(), first]).is_err());
}
#[test]
fn inaccessible_or_absent_views_retain_coverage_without_inventing_empty_success() {
    let mut source = source(&[]);
    let SourceObservation::Jobs { snapshot, metadata } = &mut source.observation else { panic!() };
    metadata.views[2].state = TimescaleJobViewState::PermissionDenied;
    metadata.run_view = None;
    snapshot.sources[1] =
        StateSourceCoverage::unavailable(TIMESCALE_JOB_RUNS_SOURCE.into(), ErrorCode::PermissionDenied, None);
    assert!(TimescaleJobsSnapshot::from_source(&source, vec![]).unwrap().is_valid());
    assert_eq!(source.observation.snapshot().unwrap().sources[1].state, SourceState::Unavailable);
    let SourceObservation::Jobs { metadata, .. } = &mut source.observation else { panic!() };
    metadata.extension = None;
    for view in &mut metadata.views {
        view.identity = None;
        view.state = TimescaleJobViewState::Missing;
    }
    metadata.run_visibility = TimescaleJobRunVisibility::Unknown;
    metadata.successful_runs_logged = None;
    assert!(metadata.is_valid());
}

fn state() -> TimescaleJobState {
    TimescaleJobState {
        position: 1,
        observed_at: "2020-01-02T12:00:00.5Z".into(),
        job_id: "1000".into(),
        procedure: TimescaleJobProcedure { schema: "public".into(), name: "/private/job-procedure-canary".into() },
        owner: "private-job-owner-canary".into(),
        scheduled: true,
        schedule_interval: "1 mon".into(),
        max_runtime: "01:00:00".into(),
        max_retries: "-1".into(),
        retry_period: "00:05:00".into(),
        fixed_schedule: Some(true),
        configuration_check: Some(TimescaleJobProcedure {
            schema: "public".into(),
            name: "/private/job-check-canary".into(),
        }),
        initial_start: Some(TimescaleJobTimestamp::NegativeInfinity),
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
    }
}

#[test]
fn sharing_withholds_the_read_endpoint_owner_and_paths_but_preserves_valid_exact_native_measurements() {
    let point = state();
    let row = OperationalObservation {
        source: TIMESCALE_JOBS_SOURCE.into(),
        key: point.key().unwrap(),
        fact: OperationalFact::TimescaleJobState(Box::new(point)),
    };
    let mut source = source(&[]);
    let SourceObservation::Jobs { snapshot, .. } = &mut source.observation else { panic!() };
    snapshot.records = 1;
    snapshot.sources[0] = StateSourceCoverage::collected(TIMESCALE_JOBS_SOURCE.into(), 1);
    let original = TimescaleJobsSnapshot::from_source(&source, vec![row]).unwrap();
    let mut snapshot: crate::Snapshot =
        serde_json::from_str(include_str!("../../../../../fixtures/postgres-demo.json")).unwrap();
    snapshot.operations.jobs =
        Some(Box::new(JobsSnapshot::Timescale { snapshot: Box::new(original.clone()), read_complete: true }));
    let shared = crate::redact::redact_snapshot(&snapshot);
    let read = shared.operations.jobs.as_ref().unwrap();
    assert!(read.is_valid());
    let json = serde_json::to_string(read).unwrap();
    for private in
        ["owned.invalid", "private-job-owner-canary", "/private/job-procedure-canary", "/private/job-check-canary"]
    {
        assert!(!json.contains(private), "{private}");
    }
    assert!(json.contains("9007199254740993"));
    assert!(json.contains("1 mon"));
    assert!(json.contains("negative-infinity"));
    assert_eq!(
        serde_json::to_value(read).unwrap(),
        serde_json::to_value(crate::redact::redact_snapshot(&shared).operations.jobs.unwrap()).unwrap()
    );
    let JobsSnapshot::Timescale { snapshot: native, read_complete } = snapshot.operations.jobs.as_deref().unwrap()
    else {
        panic!()
    };
    assert_eq!(native.as_ref(), &original);
    assert!(*read_complete);
}

#[test]
fn jobs_require_the_native_profile_subject_and_a_typed_header_even_for_empty_reads() {
    let mut request = CollectionRequest {
        subject: CollectionSubject::Database,
        selection: CollectionSelection::Snapshot { dataset: SnapshotDataset::Jobs },
        budget: CollectionBudget::default(),
    };
    let source = source(&[]);
    assert!(request.validate().is_ok());
    assert!(source.matches_request(&request));
    let mut bare = source.clone();
    bare.observation = SourceObservation::Snapshot(source.observation.snapshot().unwrap().clone());
    assert!(!bare.observation.covers_request());
    assert!(!bare.matches_request(&request));
    let mut oversized = source.clone();
    let SourceObservation::Jobs { snapshot, .. } = &mut oversized.observation else { panic!() };
    snapshot.records = MAX_TIMESCALE_JOB_OBSERVATIONS as u32 + 1;
    snapshot.sources[1] = StateSourceCoverage::observed(TIMESCALE_JOB_RUNS_SOURCE.into(), snapshot.records, false);
    assert!(!oversized.observation.covers_request());
    assert!(
        !oversized.matches_request(&CollectionRequest { budget: CollectionBudget::upper_limits(), ..request.clone() })
    );
    request.subject = CollectionSubject::Shared {
        resource: SharedResourceIdentity {
            engine: Engine::Postgres,
            kind: SharedResourceKind::Instance,
            namespace: NativeIdentity::new("owned"),
            native: NativeIdentity::new("instance"),
        },
    };
    assert!(request.validate().is_ok(), "the closed native profile determines whether an instance subject applies");
    assert!(!source.matches_request(&request));
}

#[test]
fn stored_labels_require_the_verified_original_view_contract_and_never_current_definition_equality() {
    let mut point = run();
    point.native_run_id = None;
    point.procedure = Some(TimescaleReportedJobProcedure {
        label: TimescaleJobProcedure { schema: "reported".into(), name: "stored_label".into() },
        origin: TimescaleJobProcedureOrigin::Stored,
    });
    let original = row(point);
    let mut source = source(std::slice::from_ref(&original));
    let SourceObservation::Jobs { metadata, .. } = &mut source.observation else { panic!() };
    metadata.run_view = Some(TimescaleJobViewKind::Errors);
    assert!(TimescaleJobsSnapshot::from_source(&source, vec![original.clone()]).is_ok());
    let SourceObservation::Jobs { metadata, .. } = &mut source.observation else { panic!() };
    metadata.extension.as_mut().unwrap().qualifiers.insert("version".into(), "2.99.0".into());
    assert!(TimescaleJobsSnapshot::from_source(&source, vec![original.clone()]).is_err());
    let mut unknown = original;
    let OperationalFact::TimescaleJobRun(point) = &mut unknown.fact else { panic!() };
    point.procedure.as_mut().unwrap().origin = TimescaleJobProcedureOrigin::Unknown;
    assert!(TimescaleJobsSnapshot::from_source(&source, vec![unknown]).is_ok());
    assert_eq!(
        TimescaleJobProcedureOrigin::for_view("2.31.0", TimescaleJobViewKind::History),
        TimescaleJobProcedureOrigin::StoredOrCurrent
    );
    assert_eq!(
        TimescaleJobProcedureOrigin::for_view("2.99.0", TimescaleJobViewKind::History),
        TimescaleJobProcedureOrigin::Unknown
    );
}

#[test]
fn configuration_function_labels_are_required_nullable_and_do_not_supply_a_new_job_identity() {
    let original = state();
    assert!(original.is_valid());
    let value = serde_json::to_value(&original).unwrap();
    let mut missing = value.clone();
    missing.as_object_mut().unwrap().remove("configuration_check");
    assert!(serde_json::from_value::<TimescaleJobState>(missing).is_err());
    let mut none = original.clone();
    none.configuration_check = None;
    assert!(none.is_valid());
    assert_eq!(none.key().unwrap(), original.key().unwrap());
    assert_eq!(serde_json::from_value::<TimescaleJobState>(serde_json::to_value(&none).unwrap()).unwrap(), none);
    for (schema, name) in [
        ("".into(), "check".into()),
        ("public".into(), "".into()),
        ("public".into(), "x".repeat(MAX_TIMESCALE_JOB_LABEL_BYTES + 1)),
    ] {
        let mut invalid = original.clone();
        invalid.configuration_check = Some(TimescaleJobProcedure { schema, name });
        assert!(!invalid.is_valid());
    }
}
