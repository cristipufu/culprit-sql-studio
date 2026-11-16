use super::super::super::*;
use crate::{connect::Connector, read::NativeRead, rows::quote_ident};
use culprit_collector::capture::{CollectionCancellation, SourceFactory, collect_once};
use culprit_core::{ConnectionSpec, Engine, Provider, SourceState};
use culprit_read::admission::SourceGate;
use futures_util::{FutureExt, future::LocalBoxFuture};
use std::{
    collections::BTreeSet,
    panic::AssertUnwindSafe,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};
use tokio_postgres::{Client, Row as NativeRow, types::ToSql};

const CONTROL_TIMEOUT: Duration = Duration::from_secs(3);
const REPLAY_TIMEOUT: Duration = Duration::from_secs(8);
const CASE_TIMEOUT: Duration = Duration::from_secs(40);
const DATABASE: &str = "culprit_replication";

pub(super) fn budget() -> CollectionBudget {
    CollectionBudget {
        page_rows: 1,
        max_snapshot_rows: 32,
        page_bytes: 64 * 1024,
        total_bytes: 256 * 1024,
        max_pages: 64,
        max_queries: 4,
        statement_timeout_ms: 1_000,
        total_timeout_ms: 5_000,
        ..Default::default()
    }
}

pub(super) async fn run_case(case: for<'a> fn(&'a mut Fixture) -> LocalBoxFuture<'a, ()>) {
    let _serial = super::SERIAL.lock().await;
    let mut fixture = Fixture::open().await;
    let result = tokio::time::timeout(CASE_TIMEOUT, AssertUnwindSafe(case(&mut fixture)).catch_unwind()).await;
    let cleanup = fixture.cleanup().await;
    if !cleanup.is_empty() {
        eprintln!("owned replication cleanup incomplete: {cleanup:?}");
    }
    match result {
        Ok(Ok(())) => {}
        Ok(Err(panic)) => std::panic::resume_unwind(panic),
        Err(_) => panic!("owned replication conformance exceeded its case deadline"),
    }
    assert!(cleanup.is_empty(), "{cleanup:?}");
}

struct Control {
    client: Option<Client>,
    in_flight: bool,
}
impl Control {
    async fn open(specification: &ConnectionSpec) -> Self {
        let connector = Connector::new(specification).unwrap();
        let session = tokio::time::timeout(CONTROL_TIMEOUT, connector.connect_unconfigured())
            .await
            .expect("owned control connection deadline")
            .expect("owned control connection");
        let mut control = Self { client: Some(session.client), in_flight: false };
        control
            .execute("SET statement_timeout='1s';SET lock_timeout='1s';SET standard_conforming_strings=on")
            .await
            .unwrap();
        control
    }

    fn ready(&self) -> bool {
        self.client.is_some() && !self.in_flight
    }

    async fn execute(&mut self, sql: &str) -> std::result::Result<(), &'static str> {
        if !self.ready() {
            return Err("owned control connection is not synchronized");
        }
        self.in_flight = true;
        let result = tokio::time::timeout(
            CONTROL_TIMEOUT,
            self.client.as_ref().unwrap().batch_execute(&crate::rows::tagged(sql)),
        )
        .await;
        if matches!(result, Ok(Ok(()))) {
            self.in_flight = false;
            Ok(())
        } else {
            self.client.take();
            Err("owned control statement did not complete")
        }
    }

    async fn one(
        &mut self,
        sql: &str,
        parameters: &[&(dyn ToSql + Sync)],
    ) -> std::result::Result<NativeRow, &'static str> {
        if !self.ready() {
            return Err("owned control connection is not synchronized");
        }
        self.in_flight = true;
        let result = tokio::time::timeout(
            CONTROL_TIMEOUT,
            self.client.as_ref().unwrap().query_one(&crate::rows::tagged(sql), parameters),
        )
        .await;
        match result {
            Ok(Ok(row)) => {
                self.in_flight = false;
                Ok(row)
            }
            _ => {
                self.client.take();
                Err("owned control query did not complete")
            }
        }
    }
}

struct RestoreFunction {
    name: &'static str,
    public_execute: bool,
}

pub(super) struct Fixture {
    primary: Control,
    standby: Control,
    pub(super) primary_spec: ConnectionSpec,
    pub(super) standby_spec: ConnectionSpec,
    pub(super) version: u32,
    pub(super) application: String,
    nonce: String,
    roles: Vec<String>,
    functions: Vec<RestoreFunction>,
    observers: Vec<ReadSession>,
    providers: Vec<crate::PostgresProvider>,
    started: Instant,
}
impl Fixture {
    async fn open() -> Self {
        let nonce = std::env::var("CULPRIT_PG_REPLICATION_NONCE").expect("explicit owned replication topology");
        assert!(nonce.len() == 24 && nonce.bytes().all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()));
        let specification = |variable: &str| {
            let port = std::env::var(variable).unwrap().parse::<u16>().unwrap();
            assert!(port >= 1024 && port != 5432, "owned ephemeral loopback port only");
            ConnectionSpec {
                engine: Some(Engine::Postgres),
                host: "127.0.0.1".into(),
                port: Some(port),
                database: Some(DATABASE.into()),
                user: Some("postgres".into()),
                password: Some(std::env::var("CULPRIT_PG_REPLICATION_PASSWORD").unwrap()),
                options: [("sslmode".into(), "disable".into())].into(),
                ..Default::default()
            }
        };
        let primary_spec = specification("CULPRIT_PG_REPLICATION_PRIMARY_PORT");
        let standby_spec = specification("CULPRIT_PG_REPLICATION_STANDBY_PORT");
        assert_ne!(primary_spec.port, standby_spec.port);
        let mut primary = Control::open(&primary_spec).await;
        let mut standby = Control::open(&standby_spec).await;
        let marker = format!("culprit_replication_{nonce}");
        let context = "SELECT current_database() AS database,current_setting('cluster_name') AS marker,pg_is_in_recovery() AS recovery,current_setting('server_version_num')::integer AS version,(pg_control_system()).system_identifier::text AS system_identifier";
        let before = primary.one(context, &[]).await.unwrap();
        let after = standby.one(context, &[]).await.unwrap();
        for (row, recovery) in [(&before, false), (&after, true)] {
            assert_eq!(row.get::<_, String>("database"), DATABASE);
            assert_eq!(row.get::<_, String>("marker"), marker);
            assert_eq!(row.get::<_, bool>("recovery"), recovery);
        }
        let version = before.get::<_, i32>("version") as u32;
        assert!(version >= 140_000, "this native pair qualifies the supported pause-state API, not older layouts");
        assert_eq!(version, after.get::<_, i32>("version") as u32);
        assert_eq!(before.get::<_, String>("system_identifier"), after.get::<_, String>("system_identifier"));
        Self {
            primary,
            standby,
            primary_spec,
            standby_spec,
            version,
            application: marker,
            nonce,
            roles: Vec::new(),
            functions: Vec::new(),
            observers: Vec::new(),
            providers: Vec::new(),
            started: Instant::now(),
        }
    }

    pub(super) async fn create_role(&mut self, suffix: &str, statistics: bool) -> ConnectionSpec {
        assert!(suffix.bytes().all(|byte| byte.is_ascii_lowercase()));
        let role = format!("culprit_repl_{}_{}", self.nonce, suffix);
        self.roles.push(role.clone()); // Retain cleanup ownership before the mutating request is polled.
        let password = self.primary_spec.password.as_ref().unwrap().replace('\'', "''");
        self.primary.execute(&format!("CREATE ROLE {} LOGIN PASSWORD '{password}'", quote_ident(&role))).await.unwrap();
        self.primary
            .execute(&format!(
                "GRANT pg_read_all_settings TO {};GRANT CONNECT ON DATABASE {} TO {}",
                quote_ident(&role),
                quote_ident(DATABASE),
                quote_ident(&role)
            ))
            .await
            .unwrap();
        if statistics {
            self.primary.execute(&format!("GRANT pg_read_all_stats TO {}", quote_ident(&role))).await.unwrap();
        }
        self.wait_for_replay().await;
        let mut specification = self.standby_spec.clone();
        specification.user = Some(role);
        specification
    }

    pub(super) async fn deny_pause_functions(&mut self) {
        for name in ["pg_catalog.pg_is_wal_replay_paused()", "pg_catalog.pg_get_wal_replay_pause_state()"] {
            let row = self.primary.one("SELECT EXISTS(SELECT 1 FROM pg_catalog.pg_proc p CROSS JOIN LATERAL pg_catalog.aclexplode(COALESCE(p.proacl,pg_catalog.acldefault('f',p.proowner))) a WHERE p.oid=$1::text::regprocedure AND a.grantee=0 AND a.privilege_type='EXECUTE') AS allowed", &[&name]).await.unwrap();
            self.functions.push(RestoreFunction { name, public_execute: row.get("allowed") });
            self.primary.execute(&format!("REVOKE EXECUTE ON FUNCTION {name} FROM PUBLIC")).await.unwrap();
        }
        self.wait_for_replay().await;
    }

    pub(super) async fn wait_for_replay(&mut self) {
        let position = self
            .primary
            .one("SELECT pg_catalog.pg_current_wal_lsn()::text AS position", &[])
            .await
            .unwrap()
            .get::<_, String>("position");
        let deadline = tokio::time::Instant::now() + REPLAY_TIMEOUT;
        loop {
            let row = self
                .standby
                .one("SELECT pg_catalog.pg_last_wal_replay_lsn() >= $1::text::pg_lsn AS caught_up", &[&position])
                .await
                .unwrap();
            if row.get::<_, Option<bool>>("caught_up") == Some(true) {
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "owned standby must replay the exact fixture catalog barrier"
            );
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }

    pub(super) async fn scan(
        &mut self,
        specification: &ConnectionSpec,
    ) -> (ReplicationSnapshot, Vec<culprit_core::Replica>, bool) {
        self.observers.push(ReadSession::new(Connector::new(specification).unwrap(), budget()));
        let reader = self.observers.last_mut().unwrap();
        let result = native::replication::acquire(reader, None).await;
        let shutdown = reader.close().await;
        assert_eq!(shutdown, SourceShutdown::Quiescent);
        assert_eq!(reader.queries(), 3, "one counted setup plus the original context and native relation reads");
        let metrics = reader.metrics().unwrap();
        assert!(metrics.is_valid() && !metrics.has_unconfirmed_work());
        assert_eq!(metrics.active_tasks, 0);
        eprintln!(
            "replication scan native_queries={} metrics={}",
            reader.queries(),
            serde_json::to_string(&metrics).unwrap()
        );
        let read = result.unwrap();
        (read.snapshot, read.replicas, read.complete_visibility)
    }

    pub(super) async fn capture(
        &mut self,
        specification: &ConnectionSpec,
    ) -> (CollectionManifest, Vec<ExportedRecord>) {
        self.providers.push(crate::PostgresProvider::new(specification).unwrap());
        let provider = self.providers.last().unwrap();
        let directory = tempfile::tempdir().unwrap();
        let output = directory.path().join("capture");
        let result = collect_once(
            &SourceGate::default(),
            &Factory(provider),
            request(),
            &output,
            &CollectionCancellation::default(),
        )
        .await;
        assert_eq!(provider.source_shutdown(), SourceShutdown::Quiescent);
        let manifest = result.unwrap();
        assert_eq!(manifest.status, CollectionStatus::Complete);
        assert_eq!(manifest.source_queries, 4, "setup, original metadata, one native relation, final metadata");
        let metrics = manifest.read_metrics.as_ref().unwrap();
        assert!(metrics.is_valid() && !metrics.has_unconfirmed_work());
        assert_eq!(metrics.active_tasks, 0);
        assert_eq!(metrics.templates.iter().map(|metric| metric.completed_tasks).sum::<u32>(), manifest.source_queries);
        let records = manifest
            .files
            .iter()
            .filter(|file| file.stream == TelemetryStream::ReplicationSamples)
            .flat_map(|file| {
                std::fs::read_to_string(output.join(&file.file))
                    .unwrap()
                    .lines()
                    .map(|line| serde_json::from_str::<ExportedRecord>(line).unwrap())
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        let source = manifest.description.as_ref().unwrap();
        assert_eq!(source.observation.snapshot().unwrap().records as usize, records.len());
        assert!(
            source
                .observation
                .snapshot()
                .unwrap()
                .sources
                .iter()
                .all(|scope| matches!(scope.state, SourceState::Collected | SourceState::Empty))
        );
        for record in &records {
            assert_eq!(*record, ExportedRecord::new(source, &manifest.capture, record.record.clone()).unwrap());
        }
        let native_points = rows(&records);
        let snapshot = ReplicationSnapshot::from_source(source, native_points.clone()).unwrap();
        assert!(snapshot.is_valid(), "a complete native capture must cross the shared snapshot boundary");
        let declared = source.observation.snapshot().unwrap();
        assert_eq!(snapshot.sources, declared.sources);
        assert_eq!(snapshot.observations, native_points);
        let context = snapshot.context.native().expect("the original capture descriptor supplies read context");
        assert_eq!(context.target, source.target);
        assert_eq!(context.partition, source.partition, "reconstruction cannot strengthen native owner or epoch");
        assert_eq!(context.observed_from, declared.observed_from);
        assert_eq!(context.observed_until, declared.observed_until);
        assert_eq!(context.database_writable, None, "replication snapshots do not supply native updateability");
        assert_eq!(source.continuity, SourceContinuity::Unverified);
        eprintln!(
            "replication capture native_queries={} metrics={}",
            manifest.source_queries,
            serde_json::to_string(metrics).unwrap()
        );
        (manifest, records)
    }

    pub(super) async fn denied_capture(&mut self, specification: &ConnectionSpec) {
        self.providers.push(crate::PostgresProvider::new(specification).unwrap());
        let provider = self.providers.last().unwrap();
        let directory = tempfile::tempdir().unwrap();
        let output = directory.path().join("denied");
        let result = collect_once(
            &SourceGate::default(),
            &Factory(provider),
            request(),
            &output,
            &CollectionCancellation::default(),
        )
        .await;
        assert_eq!(provider.source_shutdown(), SourceShutdown::Quiescent);
        assert_eq!(result.unwrap_err().code, ErrorCode::PermissionDenied);
        let manifest: CollectionManifest =
            serde_json::from_slice(&std::fs::read(output.join("manifest.json")).unwrap()).unwrap();
        assert!(matches!(manifest.status, CollectionStatus::Incomplete(_)));
        assert_eq!(manifest.source_queries, 2, "statistics permission stops before the native payload query");
        assert!(manifest.files.iter().all(|file| file.rows == 0));
        assert!(!manifest.read_metrics.as_ref().unwrap().has_unconfirmed_work());
    }

    pub(super) async fn stopped_capture(
        &mut self,
        specification: &ConnectionSpec,
        request: CollectionRequest,
        expected: CollectionStop,
        cancel_after_first_page: bool,
    ) {
        self.providers.push(crate::PostgresProvider::new(specification).unwrap());
        let provider = self.providers.last().unwrap();
        let cancellation = CollectionCancellation::default();
        let finishes = Arc::new(AtomicUsize::new(0));
        let closes = Arc::new(AtomicUsize::new(0));
        let factory = ObservedFactory {
            provider,
            cancellation: cancellation.clone(),
            finishes: finishes.clone(),
            closes: closes.clone(),
            cancel_after_first_page,
        };
        let directory = tempfile::tempdir().unwrap();
        let output = directory.path().join("stopped");
        let result = collect_once(&SourceGate::default(), &factory, request.clone(), &output, &cancellation).await;
        assert_eq!(provider.source_shutdown(), SourceShutdown::Quiescent);
        let manifest = match result {
            Ok(manifest) => manifest,
            Err(error) => {
                assert_eq!(CollectionStop::from_error(&error), expected);
                serde_json::from_slice::<CollectionManifest>(&std::fs::read(output.join("manifest.json")).unwrap())
                    .unwrap()
            }
        };
        assert_eq!(manifest.status, CollectionStatus::Incomplete(expected));
        assert_eq!(manifest.source_queries, 3, "no new payload read or native final recheck after the stop");
        assert_eq!(closes.load(Ordering::SeqCst), 1);
        assert_eq!(finishes.load(Ordering::SeqCst), usize::from(expected == CollectionStop::QueryLimit));
        assert!(
            manifest
                .description
                .as_ref()
                .is_some_and(|source| matches!(source.observation, SourceObservation::PagedSnapshot(_)))
        );
        assert!(manifest.files.iter().all(|file| file.rows <= request.budget.max_snapshot_rows));
        let metrics = manifest.read_metrics.as_ref().unwrap();
        assert_eq!(metrics.active_tasks, 0);
        assert!(metrics.is_valid() && !metrics.has_unconfirmed_work());
    }

    pub(super) async fn slot_columns(&mut self) -> BTreeSet<String> {
        let row = self.primary.one("SELECT pg_catalog.array_to_json(pg_catalog.array_agg(attname ORDER BY attnum))::text AS columns FROM pg_catalog.pg_attribute WHERE attrelid='pg_catalog.pg_replication_slots'::regclass AND attnum>0 AND NOT attisdropped", &[]).await.unwrap();
        serde_json::from_str(&row.get::<_, String>("columns")).unwrap()
    }

    async fn cleanup(&mut self) -> Vec<&'static str> {
        let mut errors = Vec::new();
        for observer in &mut self.observers {
            if observer.close().await != SourceShutdown::Quiescent {
                errors.push("native observer shutdown unconfirmed");
            }
        }
        if self.providers.iter().any(|provider| provider.source_shutdown() != SourceShutdown::Quiescent) {
            errors.push("native capture shutdown unconfirmed");
        }
        if !self.primary.ready() || !errors.is_empty() {
            self.primary.client.take();
            errors.push("native resource cleanup withheld after uncertain work; supervisor must remove the owned pair");
            return errors;
        }
        for function in self.functions.iter().rev() {
            let command = if function.public_execute {
                format!("GRANT EXECUTE ON FUNCTION {} TO PUBLIC", function.name)
            } else {
                format!("REVOKE EXECUTE ON FUNCTION {} FROM PUBLIC", function.name)
            };
            if self.primary.execute(&command).await.is_err() {
                errors.push("pause-function privileges were not restored");
            }
        }
        for role in self.roles.iter().rev() {
            if self
                .primary
                .execute(&format!(
                    "REVOKE CONNECT ON DATABASE {} FROM {};DROP ROLE IF EXISTS {}",
                    quote_ident(DATABASE),
                    quote_ident(role),
                    quote_ident(role)
                ))
                .await
                .is_err()
            {
                errors.push("owned test role was not removed");
            }
        }
        eprintln!(
            "replication native conformance elapsed_ms={} cleanup_errors={}; local evidence, source cost remains Unqualified",
            self.started.elapsed().as_secs_f64() * 1000.0,
            errors.len()
        );
        errors
    }
}

pub(super) fn request() -> CollectionRequest {
    CollectionRequest {
        subject: CollectionSubject::Database,
        selection: CollectionSelection::Snapshot { dataset: SnapshotDataset::Replication },
        budget: budget(),
    }
}

struct Factory<'a>(&'a crate::PostgresProvider);
#[async_trait::async_trait]
impl SourceFactory for Factory<'_> {
    async fn open(&self, request: &CollectionRequest) -> Result<Box<dyn TelemetrySource>> {
        let source = self.0.telemetry_source(request)?;
        assert_eq!(source.query_count(), 0, "source construction must remain lazy");
        Ok(source)
    }
}

struct ObservedFactory<'a> {
    provider: &'a crate::PostgresProvider,
    cancellation: CollectionCancellation,
    finishes: Arc<AtomicUsize>,
    closes: Arc<AtomicUsize>,
    cancel_after_first_page: bool,
}
#[async_trait::async_trait]
impl SourceFactory for ObservedFactory<'_> {
    async fn open(&self, request: &CollectionRequest) -> Result<Box<dyn TelemetrySource>> {
        let inner = self.provider.telemetry_source(request)?;
        assert_eq!(inner.query_count(), 0);
        Ok(Box::new(ObservedSource {
            inner,
            cancellation: self.cancellation.clone(),
            finishes: self.finishes.clone(),
            closes: self.closes.clone(),
            cancel_after_first_page: self.cancel_after_first_page,
        }))
    }
}
struct ObservedSource {
    inner: Box<dyn TelemetrySource>,
    cancellation: CollectionCancellation,
    finishes: Arc<AtomicUsize>,
    closes: Arc<AtomicUsize>,
    cancel_after_first_page: bool,
}
#[async_trait::async_trait]
impl TelemetrySource for ObservedSource {
    async fn describe(&mut self) -> Result<CollectionDescription> {
        self.inner.describe().await
    }
    fn query_count(&self) -> u32 {
        self.inner.query_count()
    }
    fn read_metrics(&self) -> Option<ReadWorkMetrics> {
        self.inner.read_metrics()
    }
    async fn read_page(&mut self, stream: TelemetryStream, cursor: Option<&str>) -> Result<CollectionRead> {
        let read = self.inner.read_page(stream, cursor).await?;
        if self.cancel_after_first_page && matches!(&read, CollectionRead::Page { records, .. } if !records.is_empty())
        {
            self.cancel_after_first_page = false;
            self.cancellation.cancel();
        }
        Ok(read)
    }
    async fn finish(&mut self) -> Result<SourceCompletion> {
        self.finishes.fetch_add(1, Ordering::SeqCst);
        self.inner.finish().await
    }
    async fn close(&mut self) -> SourceShutdown {
        self.closes.fetch_add(1, Ordering::SeqCst);
        self.inner.close().await
    }
}

pub(super) fn native_sample(row: &ReplicationObservation) -> &NativeReplicationSample {
    let ReplicationFact::Native(sample) = &row.fact else { panic!("native PostgreSQL replication point") };
    sample
}

pub(super) fn point<'a>(rows: &'a [ReplicationObservation], source: &str) -> &'a NativeReplicationSample {
    let mut matching = rows.iter().filter(|row| row.source == source);
    let row = matching.next().expect("one native source point");
    assert!(matching.next().is_none(), "native source point must be unambiguous");
    native_sample(row)
}

pub(super) fn rows(records: &[ExportedRecord]) -> Vec<ReplicationObservation> {
    records
        .iter()
        .map(|record| {
            let TelemetryRecord::Replication(row) = &record.record else { panic!("replication stream") };
            row.clone()
        })
        .collect()
}
