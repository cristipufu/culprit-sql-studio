//! An owned, nonce-qualified server only. Fixture writers are separate from the read-only observer.
use super::*;
use crate::{
    connect::Connector,
    read::{NativeRead, ReadSession},
    rows::quote_ident,
};
use culprit_core::{ConnectionSpec, Engine};
use futures_util::FutureExt;
use std::{panic::AssertUnwindSafe, time::Duration};
use tokio::task::JoinHandle;
use tokio_postgres::Client;

struct RecordingReader {
    inner: ReadSession,
    calls: Vec<ObservedRead>,
}
struct ObservedRead {
    sql: String,
    limit: usize,
    client_ms: f64,
    retained_rows: Option<usize>,
}
#[async_trait::async_trait]
impl QueryRead for RecordingReader {
    async fn query(&mut self, sql: &str, limit: usize) -> Result<Vec<Row>> {
        let started = std::time::Instant::now();
        let result = self.inner.query(sql, limit).await;
        self.calls.push(ObservedRead {
            sql: sql.into(),
            limit,
            client_ms: started.elapsed().as_secs_f64() * 1000.0,
            retained_rows: result.as_ref().ok().map(Vec::len),
        });
        result
    }
}

struct Fixture {
    admin: Client,
    specification: ConnectionSpec,
    selected: String,
    peer: String,
    role: String,
    actor: Option<Client>,
    holder: Option<Client>,
    unrelated: Vec<Client>,
    observer: Option<RecordingReader>,
    waiting: Option<JoinHandle<std::result::Result<(), tokio_postgres::Error>>>,
    cancel: Option<tokio_postgres::CancelToken>,
}
impl Fixture {
    async fn open() -> Self {
        let nonce = std::env::var("CULPRIT_PG_ACTIVITY_FIXTURE_NONCE").expect("owned native fixture nonce");
        assert!(nonce.len() == 24 && nonce.bytes().all(|byte| byte.is_ascii_hexdigit()));
        assert_eq!(std::env::var("CULPRIT_SMOKE_ENGINE").unwrap(), "postgres");
        let database = std::env::var("CULPRIT_PREVIEW_PG_DATABASE").unwrap();
        assert!(matches!(database.as_str(), "culprit_smoke" | "culprit_timescale"));
        let port = std::env::var("CULPRIT_PREVIEW_PG_PORT").unwrap().parse().unwrap();
        assert!(port >= 1024 && port != 5432, "owned ephemeral loopback port only");
        let specification = ConnectionSpec {
            engine: Some(Engine::Postgres),
            host: "127.0.0.1".into(),
            port: Some(port),
            database: Some(database),
            user: Some("postgres".into()),
            password: Some(std::env::var("CULPRIT_PREVIEW_PG_PASSWORD").unwrap()),
            options: [("sslmode".into(), "disable".into())].into(),
            ..Default::default()
        };
        let admin = Connector::new(&specification).unwrap().connect_unconfigured().await.unwrap().client;
        let guard = admin
            .query_one(
                "SELECT current_setting('culprit.fixture_nonce',true),current_setting('server_version_num')::integer",
                &[],
            )
            .await
            .unwrap();
        assert_eq!(
            guard.get::<_, Option<String>>(0).as_deref(),
            Some(nonce.as_str()),
            "server startup must prove the same owned fixture namespace"
        );
        assert!(guard.get::<_, i32>(1) >= 140_000);
        admin.batch_execute("SET statement_timeout='3s';SET lock_timeout='1s'").await.unwrap();
        Self {
            admin,
            specification,
            selected: format!("culprit_activity_{nonce}_selected"),
            peer: format!("culprit_activity_{nonce}_peer"),
            role: format!("culprit_activity_{nonce}"),
            actor: None,
            holder: None,
            unrelated: Vec::new(),
            observer: None,
            waiting: None,
            cancel: None,
        }
    }

    fn route(&self, database: &str) -> ConnectionSpec {
        let mut specification = self.specification.clone();
        specification.database = Some(database.into());
        specification
    }

    async fn prepare(&mut self) {
        let started = std::time::Instant::now();
        self.admin.batch_execute(&format!("CREATE DATABASE {}", quote_ident(&self.selected))).await.unwrap();
        self.admin.batch_execute(&format!("CREATE DATABASE {}", quote_ident(&self.peer))).await.unwrap();
        self.admin.batch_execute(&format!("CREATE ROLE {} NOLOGIN", quote_ident(&self.role))).await.unwrap();
        let selected = Connector::new(&self.route(&self.selected)).unwrap();
        let peer = Connector::new(&self.route(&self.peer)).unwrap();
        self.actor = Some(selected.connect_unconfigured().await.unwrap().client);
        self.holder = Some(peer.connect_unconfigured().await.unwrap().client);
        self.actor
            .as_ref()
            .unwrap()
            .batch_execute(
                "SET application_name='fixture-selected-waiter';SET statement_timeout='8s';SET lock_timeout='7s'",
            )
            .await
            .unwrap();
        self.holder
            .as_ref()
            .unwrap()
            .batch_execute(
                "SET application_name='fixture-cross-database-holder';SET statement_timeout='3s';SET lock_timeout='1s'",
            )
            .await
            .unwrap();
        for _ in 0..6 {
            let client = peer.connect_unconfigured().await.unwrap().client;
            client.batch_execute("SET application_name='fixture-unrelated-peer'").await.unwrap();
            self.unrelated.push(client);
        }
        let budget = CollectionBudget {
            max_snapshot_rows: 2,
            page_rows: 2,
            max_queries: 24,
            statement_timeout_ms: 1_000,
            total_timeout_ms: 10_000,
            max_sql_bytes: 4096,
            ..Default::default()
        };
        self.observer = Some(RecordingReader { inner: ReadSession::new(selected, budget), calls: Vec::new() });
        assert_eq!(self.observer.as_ref().unwrap().inner.queries(), 0);
        eprintln!(
            "setup client_ms={} databases=2 role=1 actor_sessions={} observer_construct_native_queries=0 server_cpu=unmeasured",
            started.elapsed().as_secs_f64() * 1000.0,
            self.unrelated.len() + 2
        );
    }

    async fn prove_scope_and_blocking(&mut self) {
        let actor = self.actor.as_ref().unwrap();
        let waiter_pid: i32 = actor.query_one("SELECT pg_backend_pid()", &[]).await.unwrap().get(0);
        let owner_pid: i32 =
            self.holder.as_ref().unwrap().query_one("SELECT pg_backend_pid()", &[]).await.unwrap().get(0);
        let observer = self.observer.as_mut().unwrap();
        let before =
            super::super::super::metadata::read(observer, "127.0.0.1", self.specification.port.unwrap()).await.unwrap();
        let budget = CollectionBudget { max_snapshot_rows: 2, ..Default::default() };
        let mut healthy = observe(observer, &before, &budget).await.unwrap();
        assert_eq!(
            healthy.records.len(),
            1,
            "unrelated peer backends cannot consume the selected database's row witness"
        );
        let initial = healthy.records[0].facts.as_ref().unwrap().key.session.clone();
        assert_eq!(initial.id, waiter_pid.to_string());
        let count = observer.inner.queries();
        blockers(
            observer,
            &mut healthy,
            BlockingReadOptions {
                minimum_request_age_ms: 0,
                scope: ActivityScope::CurrentDatabase,
                version: before.version,
                database_oid: &before.database_oid,
                max_snapshot_rows: 2,
            },
        )
        .await
        .unwrap();
        assert_eq!(observer.inner.queries(), count, "healthy polls issue no blocker/lock-manager SQL");
        assert!(healthy.locks.is_empty());
        eprintln!(
            "healthy root native rows={} unrelated peers={} extra blocker queries=0",
            healthy.records.len(),
            self.unrelated.len() + 1
        );

        let contention_started = std::time::Instant::now();
        self.holder
            .as_ref()
            .unwrap()
            .batch_execute(&format!("BEGIN;ALTER ROLE {} CONNECTION LIMIT 2", quote_ident(&self.role)))
            .await
            .unwrap();
        let actor = self.actor.take().unwrap();
        self.cancel = Some(actor.cancel_token());
        let sql = format!("BEGIN;ALTER ROLE {} CONNECTION LIMIT 3;ROLLBACK", quote_ident(&self.role));
        self.waiting = Some(tokio::spawn(async move { actor.batch_execute(&sql).await }));
        let readiness_queries = wait_until_blocked(&self.admin, waiter_pid, owner_pid).await;
        eprintln!(
            "setup controlled_contention client_ms={} readiness_queries={} actual_role_writers=2 server_cpu=unmeasured",
            contention_started.elapsed().as_secs_f64() * 1000.0,
            readiness_queries
        );

        let observer = self.observer.as_mut().unwrap();
        let mut blocked = observe(observer, &before, &budget).await.unwrap();
        assert_eq!(blocked.records.len(), 1, "only the selected database enters the baseline frame");
        let waiting = blocked.records[0].facts.as_ref().unwrap();
        assert_eq!(waiting.key.session, initial);
        assert!(waiting.key.request.is_some());
        let request_key = waiting.key.clone();
        blockers(
            observer,
            &mut blocked,
            BlockingReadOptions {
                minimum_request_age_ms: 0,
                scope: ActivityScope::CurrentDatabase,
                version: before.version,
                database_oid: &before.database_oid,
                max_snapshot_rows: 2,
            },
        )
        .await
        .unwrap();
        assert_eq!(blocked.records.len(), 2, "only the reached native blocker is fetched from the other database");
        let owner = blocked
            .records
            .iter()
            .find_map(|record| record.facts.as_ref().filter(|row| row.key.session.id == owner_pid.to_string()))
            .unwrap();
        assert_eq!(owner.scope, ActivityScopeBasis::OtherDatabase);
        assert_eq!(owner.database.as_deref(), Some(self.peer.as_str()));
        assert!(owner.idle && owner.key.transaction.is_some());
        let waiter = blocked.records[0].facts.as_ref().unwrap();
        assert_eq!(waiter.key, request_key);
        assert!(
            waiter
                .wait
                .as_ref()
                .unwrap()
                .owners
                .iter()
                .any(|owner_ref| owner_ref.session().as_ref() == Some(&owner.key.session))
        );
        let keys = blocked.observations().into_iter().map(|row| row.key).collect();
        enrich(observer, &mut blocked, &keys, 4096, false).await.unwrap();
        let waiter = blocked.records[0].facts.as_ref().unwrap();
        assert_eq!(waiter.key, request_key);
        assert!(
            matches!(&waiter.statement, DefinitionPayload::Available { text, .. } if text.contains("ALTER ROLE") && text.contains(&self.role))
        );
        let sessions = blocked.records.iter().map(project).collect::<Vec<_>>();
        let blocking = culprit_core::blocking::analyze(
            Engine::Postgres,
            &sessions,
            &blocked.locks,
            &unqualified_owner_ids(&blocked),
        )
        .unwrap();
        assert_eq!(blocking.waiting, 1);
        assert_eq!(blocking.chains[0].head, owner_pid.to_string());
        assert_eq!(blocking.chains[0].head_kind, culprit_core::HeadKind::IdleInTransaction);
        let after =
            super::super::super::metadata::read(observer, "127.0.0.1", self.specification.port.unwrap()).await.unwrap();
        assert!(before.same_source(&after));
        let shutdown = observer.inner.close().await;
        assert_eq!(shutdown, SourceShutdown::Quiescent);
        for (index, read) in observer.calls.iter().enumerate() {
            eprintln!(
                "observer query={} client_ms={} retained_rows={:?} retained-row-bound={} sql={}",
                index + 1,
                read.client_ms,
                read.retained_rows,
                read.limit,
                read.sql
            );
        }
        eprintln!(
            "native version={} observer explicit_queries={} owned_source_queries={} explicit_read_client_ms={} confirmed shutdown={shutdown:?} work={}",
            before.version,
            observer.calls.len(),
            observer.inner.queries(),
            observer.calls.iter().map(|read| read.client_ms).sum::<f64>(),
            serde_json::to_string(&observer.inner.metrics()).unwrap()
        );
    }

    async fn cleanup(&mut self) -> Vec<String> {
        let started = std::time::Instant::now();
        let mut errors = Vec::new();
        if let Some(observer) = &mut self.observer
            && observer.inner.close().await != SourceShutdown::Quiescent
        {
            errors.push("observer shutdown was unconfirmed".into());
        }
        if let Some(holder) = self.holder.take()
            && !matches!(
                tokio::time::timeout(Duration::from_secs(2), holder.batch_execute("ROLLBACK")).await,
                Ok(Ok(()))
            )
        {
            errors.push("owned holder rollback failed".into());
        }
        if let Some(mut waiting) = self.waiting.take() {
            match tokio::time::timeout(Duration::from_secs(2), &mut waiting).await {
                Ok(Ok(Ok(()))) => {}
                Ok(_) => errors.push("owned waiter did not finish normally".into()),
                Err(_) => {
                    if let Some(cancel) = self.cancel.take() {
                        let _ =
                            tokio::time::timeout(Duration::from_secs(1), cancel.cancel_query(tokio_postgres::NoTls))
                                .await;
                    }
                    waiting.abort();
                    let _ = waiting.await;
                    errors.push("owned waiter exceeded cleanup deadline".into());
                }
            }
        }
        self.actor.take();
        self.unrelated.clear();
        for database in [&self.selected, &self.peer] {
            if self
                .admin
                .batch_execute(&format!("DROP DATABASE IF EXISTS {} WITH (FORCE)", quote_ident(database)))
                .await
                .is_err()
            {
                errors.push("owned database removal failed".into());
            }
        }
        if self.admin.batch_execute(&format!("DROP ROLE IF EXISTS {}", quote_ident(&self.role))).await.is_err() {
            errors.push("owned role removal failed".into());
        }
        eprintln!(
            "owned database/role cleanup client_ms={} errors={}",
            started.elapsed().as_secs_f64() * 1000.0,
            errors.len()
        );
        errors
    }
}

async fn wait_until_blocked(admin: &Client, waiter: i32, owner: i32) -> u32 {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    let mut queries = 0;
    loop {
        let ready = admin.query_one("SELECT EXISTS(SELECT 1 FROM pg_catalog.pg_stat_activity WHERE pid=$1 AND wait_event_type='Lock' AND $2=ANY(pg_catalog.pg_blocking_pids(pid)))", &[&waiter, &owner]).await.unwrap().get::<_, bool>(0);
        queries += 1;
        if ready {
            return queries;
        }
        assert!(tokio::time::Instant::now() < deadline, "owned waiter must reach its native cross-database lock");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
#[ignore = "requires owned loopback PostgreSQL with matching server/runner fixture nonce"]
async fn native_selected_database_limit_and_cross_database_blocker_context_share_one_reader() {
    let mut fixture = Fixture::open().await;
    let result = AssertUnwindSafe(async {
        fixture.prepare().await;
        fixture.prove_scope_and_blocking().await;
    })
    .catch_unwind()
    .await;
    let cleanup = fixture.cleanup().await;
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
    assert!(cleanup.is_empty(), "{cleanup:?}");
}
