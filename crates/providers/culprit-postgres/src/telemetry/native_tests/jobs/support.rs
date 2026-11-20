use super::super::{case, owned_fixture};
use crate::{
    connect::Connector,
    read::{NativeRead, QueryRead, ReadSession},
};
use culprit_collector::capture::CollectionCancellation;
use culprit_core::{ConnectionSpec, SourceState, telemetry::*};
use futures_util::{FutureExt, future::LocalBoxFuture};
use std::{collections::BTreeMap, panic::AssertUnwindSafe, time::Duration};

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
    owned_fixture::specification(database, request().budget).await
}

impl Fixture {
    pub(super) async fn open() -> Self {
        Self::with_admin(|fixture, _, _| Box::pin(async move { fixture })).await
    }

    /// Keep the existing fixture reader through the oracle interval, then close it before returning or unwinding.
    pub(super) async fn with_admin<T>(
        body: for<'a> fn(Self, &'a mut ReadSession, &'a CollectionCancellation) -> LocalBoxFuture<'a, T>,
    ) -> T {
        let admin = owned_spec("culprit_timescale").await;
        // Fixture-oracle work has its own existing case deadline; the source retains its five-second budget.
        let admin_budget =
            CollectionBudget { total_timeout_ms: CASE_TIMEOUT.as_millis().try_into().unwrap(), ..request().budget };
        let mut reader = ReadSession::new(Connector::new(&admin).unwrap(), admin_budget);
        let cancellation = CollectionCancellation::default();
        let completed = case::finish(
            CASE_TIMEOUT,
            &cancellation,
            AssertUnwindSafe(async {
                let fixture = Self::read(admin, &mut reader).await;
                body(fixture, &mut reader, &cancellation).await
            })
            .catch_unwind(),
        )
        .await;
        let shutdown = reader.close().await;
        eprintln!(
            "owned jobs fixture admin_queries={} shutdown={shutdown:?}; source work is accounted separately",
            reader.queries()
        );
        let value = completed.value.unwrap_or_else(|panic| std::panic::resume_unwind(panic));
        assert_eq!(shutdown, SourceShutdown::Quiescent);
        assert_settled(&reader.metrics().unwrap(), reader.queries());
        assert!(!completed.deadline_reached, "owned jobs case exceeded its existing absolute deadline");
        value
    }

    async fn read(admin: ConnectionSpec, reader: &mut ReadSession) -> Self {
        let rows = reader
            .query("SELECT kind,job_id::text AS job_id FROM culprit_jobs_test.fixture_jobs ORDER BY kind", 3)
            .await
            .expect("supervisor-prepared paused jobs");
        let jobs =
            rows.iter().map(|row| (row.text("kind").unwrap(), row.text("job_id").unwrap())).collect::<BTreeMap<_, _>>();
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
    capture_with_cancellation(specification, request, &CollectionCancellation::default()).await
}

pub(super) async fn capture_with_cancellation(
    specification: &ConnectionSpec,
    request: CollectionRequest,
    cancellation: &CollectionCancellation,
) -> Captured {
    let captured = owned_fixture::capture(specification, request, cancellation).await;
    let source = captured.source();
    let observations = captured
        .records
        .iter()
        .map(|exported| {
            assert_eq!(exported.record.stream(), TelemetryStream::OperationalSamples);
            let TelemetryRecord::Operational(row) = &exported.record else { panic!("native job point") };
            row.clone()
        })
        .collect();
    let snapshot = TimescaleJobsSnapshot::from_source(source, observations).unwrap();
    assert!(snapshot.is_valid());
    assert_eq!(snapshot.context.target, source.target);
    assert_eq!(snapshot.context.partition, SourcePartition::Target);
    assert!(!snapshot.context.has_qualified_local_owner());
    assert_eq!(source.continuity, SourceContinuity::Unverified);
    assert_eq!(snapshot.sources, source.observation.snapshot().unwrap().sources);
    Captured { manifest: captured.manifest, snapshot }
}

pub(super) fn assert_settled(metrics: &ReadWorkMetrics, queries: u32) {
    owned_fixture::assert_settled(metrics, queries);
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
