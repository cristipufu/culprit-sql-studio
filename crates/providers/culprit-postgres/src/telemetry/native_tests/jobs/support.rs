use super::super::{Factory, spec};
use crate::{
    connect::Connector,
    read::{NativeRead, QueryRead, ReadSession},
};
use culprit_collector::capture::{CollectionCancellation, collect_once};
use culprit_core::{ConnectionSpec, Provider, SourceState, telemetry::*};
use culprit_read::admission::SourceGate;
use std::{collections::BTreeMap, time::Duration};

pub(super) const CASE_TIMEOUT: Duration = Duration::from_secs(20);
pub(super) const PRIVATE_ERROR: &str = "culprit-jobs-private-error-canary";
pub(super) const PRIVATE_CONFIG: &str = "culprit-jobs-private-config-canary";

pub(super) struct Fixture {
    pub admin: ConnectionSpec,
    pub limited: ConnectionSpec,
    pub jobs: BTreeMap<String, String>,
}

pub(super) fn request() -> CollectionRequest {
    CollectionRequest {
        subject: CollectionSubject::Database,
        selection: CollectionSelection::Snapshot { dataset: SnapshotDataset::Jobs },
        budget: crate::native::jobs::bounded_budget(&CollectionBudget::default()),
    }
}

pub(super) async fn owned_spec(database: &str) -> ConnectionSpec {
    let specification = spec();
    assert_eq!(specification.database.as_deref(), Some(database));
    assert!(specification.port.is_some_and(|port| port >= 1024 && port != 5432));
    let owner = std::env::var("CULPRIT_SMOKE_OWNER").expect("explicit owned fixture nonce");
    assert!(owner.len() == 24 && owner.bytes().all(|value| value.is_ascii_hexdigit() && !value.is_ascii_uppercase()));
    let mut reader = ReadSession::new(Connector::new(&specification).unwrap(), request().budget);
    let result = reader
        .query("SELECT current_database() AS database,pg_catalog.current_setting('cluster_name') AS marker", 1)
        .await;
    assert_eq!(reader.close().await, SourceShutdown::Quiescent);
    let rows = result.expect("bounded owned fixture admission");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].text("database").as_deref(), Some(database));
    assert_eq!(rows[0].text("marker"), Some(format!("culprit_jobs_{owner}")));
    assert_settled(&reader.metrics().unwrap(), reader.queries());
    specification
}

impl Fixture {
    pub(super) async fn open() -> Self {
        let admin = owned_spec("culprit_timescale").await;
        let mut reader = ReadSession::new(Connector::new(&admin).unwrap(), request().budget);
        let result = reader
            .query("SELECT kind,job_id::text AS job_id FROM culprit_jobs_test.fixture_jobs ORDER BY kind", 3)
            .await;
        assert_eq!(reader.close().await, SourceShutdown::Quiescent);
        assert_settled(&reader.metrics().unwrap(), reader.queries());
        let jobs = result
            .expect("supervisor-prepared paused jobs")
            .iter()
            .map(|row| (row.text("kind").unwrap(), row.text("job_id").unwrap()))
            .collect::<BTreeMap<_, _>>();
        assert_eq!(jobs.keys().map(String::as_str).collect::<Vec<_>>(), vec!["admin", "limited"]);
        assert!(jobs.values().all(|id| id.parse::<u32>().is_ok_and(|id| id > 0)));
        let mut limited = admin.clone();
        limited.user = Some(std::env::var("CULPRIT_SMOKE_LIMITED_USER").expect("owned limited login"));
        limited.password = Some(std::env::var("CULPRIT_SMOKE_LIMITED_PASSWORD").expect("owned limited secret"));
        Self { admin, limited, jobs }
    }
}

pub(super) struct Captured {
    pub manifest: CollectionManifest,
    pub snapshot: TimescaleJobsSnapshot,
}

pub(super) async fn capture(specification: &ConnectionSpec, request: CollectionRequest) -> Captured {
    let factory = Factory(crate::PostgresProvider::new(specification).unwrap());
    let directory = tempfile::tempdir().unwrap();
    let output = directory.path().join("jobs");
    let result =
        collect_once(&SourceGate::default(), &factory, request, &output, &CollectionCancellation::default()).await;
    assert_eq!(factory.0.source_shutdown(), SourceShutdown::Quiescent);
    let manifest = result.expect("native jobs capture");
    assert_eq!(
        manifest.status,
        CollectionStatus::Complete,
        "jobs description admitted: {}; native statements: {}",
        manifest.description.is_some(),
        manifest.source_queries
    );
    let source = manifest.description.as_ref().expect("original jobs source");
    let mut observations = Vec::new();
    for file in &manifest.files {
        assert_eq!(file.stream, TelemetryStream::OperationalSamples);
        for line in std::fs::read_to_string(output.join(&file.file)).unwrap().lines() {
            let exported: ExportedRecord = serde_json::from_str(line).unwrap();
            assert_eq!(exported, ExportedRecord::new(source, &manifest.capture, exported.record.clone()).unwrap());
            let TelemetryRecord::Operational(row) = exported.record else { panic!("native job point") };
            observations.push(row);
        }
    }
    let snapshot = TimescaleJobsSnapshot::from_source(source, observations).unwrap();
    assert!(snapshot.is_valid());
    assert_eq!(snapshot.context.target, source.target);
    assert_eq!(snapshot.context.partition, SourcePartition::Target);
    assert!(!snapshot.context.has_qualified_local_owner());
    assert_eq!(source.continuity, SourceContinuity::Unverified);
    assert_eq!(snapshot.sources, source.observation.snapshot().unwrap().sources);
    assert_settled(manifest.read_metrics.as_ref().unwrap(), manifest.source_queries);
    Captured { manifest, snapshot }
}

pub(super) fn assert_settled(metrics: &ReadWorkMetrics, queries: u32) {
    assert!(metrics.is_valid());
    assert!(!metrics.has_unconfirmed_work());
    assert_eq!(metrics.active_tasks, 0);
    assert_eq!(metrics.untracked_tasks, 0);
    assert_eq!(metrics.templates.iter().map(|template| template.completed_tasks).sum::<u32>(), queries);
}

pub(super) fn state<'a>(snapshot: &'a TimescaleJobsSnapshot, id: &str) -> &'a TimescaleJobState {
    let mut matches = snapshot.observations.iter().filter_map(|row| match &row.fact {
        OperationalFact::TimescaleJobState(point) if point.job_id == id => Some(point.as_ref()),
        _ => None,
    });
    let point = matches.next().expect("original fixture job state");
    assert!(matches.next().is_none());
    point
}

pub(super) fn runs<'a>(snapshot: &'a TimescaleJobsSnapshot, id: &str) -> Vec<&'a TimescaleJobRun> {
    snapshot
        .observations
        .iter()
        .filter_map(|row| match &row.fact {
            OperationalFact::TimescaleJobRun(point) if point.job_id == id => Some(point.as_ref()),
            _ => None,
        })
        .collect()
}

pub(super) fn coverage<'a>(snapshot: &'a TimescaleJobsSnapshot, source: &str) -> &'a StateSourceCoverage {
    snapshot.sources.iter().find(|entry| entry.source == source).expect("original native source coverage")
}

pub(super) fn assert_runs_are_partial(snapshot: &TimescaleJobsSnapshot) {
    let scope = coverage(snapshot, TIMESCALE_JOB_RUNS_SOURCE);
    assert_eq!(scope.state, SourceState::Partial);
    assert!(!scope.complete_scope, "retained public history is not complete absence evidence");
}
