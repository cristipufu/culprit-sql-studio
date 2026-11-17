//! A finite COPY writer belongs only to the existing nonce-admitted disposable PostgreSQL fixture.
use super::*;
use crate::telemetry::native_tests::case;
use crate::{
    connect::Connector,
    rows::{quote_ident, tagged},
};
use culprit_collector::capture::CollectionCancellation;
use culprit_core::ConnectionSpec;
use futures_util::{FutureExt, SinkExt, future::LocalBoxFuture};
use std::{panic::AssertUnwindSafe, time::Duration};
use tokio::{sync::oneshot, task::JoinHandle};
use tokio_postgres::{CancelToken, Client};

const CASE_TIMEOUT: Duration = Duration::from_secs(25);
const CONTROL_TIMEOUT: Duration = Duration::from_secs(3);
const MAX_OBSERVER_READS: usize = 40;
pub(super) const COPY_ROWS: u64 = 2048;
pub(super) const COPY_BYTES: u64 = COPY_ROWS * 2;
type CopyResult = (Client, std::result::Result<u64, tokio_postgres::Error>);

pub(super) struct Fixture {
    pub specification: ConnectionSpec,
    pub cancellation: CollectionCancellation,
    pub pid: i32,
    pub relation_oid: String,
    connector: Connector,
    schema: String,
    schema_created: bool,
    admin: Option<Client>,
    admin_in_flight: bool,
    backend_start: String,
    actor: Option<JoinHandle<CopyResult>>,
    release: Option<oneshot::Sender<()>>,
    cancel: Option<CancelToken>,
}

impl Fixture {
    pub(super) async fn run(case: &str, body: for<'a> fn(&'a mut Self) -> LocalBoxFuture<'a, ()>) {
        let mut fixture = Self::open(case).await;
        let cancellation = fixture.cancellation.clone();
        let completed =
            case::finish(CASE_TIMEOUT, &cancellation, AssertUnwindSafe(body(&mut fixture)).catch_unwind()).await;
        let settled = fixture.cleanup().await;
        eprintln!("owned COPY fixture cleanup_confirmed={settled}; native recipe cost remains unqualified");
        if let Err(panic) = completed.value {
            std::panic::resume_unwind(panic);
        }
        assert!(
            !completed.deadline_reached,
            "owned COPY fixture exceeded its absolute deadline; cleanup_confirmed={settled}"
        );
        assert!(settled, "owned COPY actor and schema cleanup must be confirmed");
    }

    async fn open(case: &str) -> Self {
        assert!(matches!(case, "observed" | "limits" | "cancelled"));
        let database = std::env::var("CULPRIT_PREVIEW_PG_DATABASE").unwrap();
        let specification = owned_fixture::specification(&database, CollectionBudget::default()).await;
        let connector = Connector::new(&specification).unwrap();
        let admin =
            tokio::time::timeout(CONTROL_TIMEOUT, connector.connect_unconfigured()).await.unwrap().unwrap().client;
        Self {
            specification,
            cancellation: CollectionCancellation::default(),
            connector,
            schema: format!("culprit_copy_{}_{}", owned_fixture::owner(), case),
            schema_created: false,
            admin: Some(admin),
            admin_in_flight: false,
            pid: 0,
            relation_oid: String::new(),
            backend_start: String::new(),
            actor: None,
            release: None,
            cancel: None,
        }
    }

    async fn execute(&mut self, sql: &str) {
        assert!(!self.admin_in_flight);
        self.admin_in_flight = true;
        let result =
            tokio::time::timeout(CONTROL_TIMEOUT, self.admin.as_ref().unwrap().batch_execute(&tagged(sql))).await;
        assert!(matches!(result, Ok(Ok(()))), "owned COPY fixture control did not acknowledge completion");
        self.admin_in_flight = false;
    }

    pub(super) async fn prepare(&mut self) {
        self.execute("SET statement_timeout='1s';SET lock_timeout='1s'").await;
        self.schema_created = true;
        self.execute(&format!(
            "CREATE SCHEMA {};CREATE TABLE {}.items(id integer NOT NULL)",
            quote_ident(&self.schema),
            quote_ident(&self.schema)
        ))
        .await;
        self.admin_in_flight = true;
        let row=tokio::time::timeout(CONTROL_TIMEOUT,self.admin.as_ref().unwrap().query_one(&tagged("SELECT pg_catalog.current_setting('server_version_num')::integer,pg_catalog.to_regclass($1)::oid::text"),&[&format!("{}.items",self.schema)])).await.unwrap().unwrap();
        self.admin_in_flight = false;
        assert!(row.get::<_, i32>(0) >= 140_000, "this owned COPY compatibility test requires PostgreSQL14+");
        self.relation_oid = row.get(1);
        let actor =
            tokio::time::timeout(CONTROL_TIMEOUT, self.connector.connect_unconfigured()).await.unwrap().unwrap().client;
        let result=tokio::time::timeout(CONTROL_TIMEOUT,actor.batch_execute(&tagged("SET statement_timeout='20s';SET lock_timeout='1s';SET idle_in_transaction_session_timeout='25s';SET application_name='fixture-copy'"))).await;
        assert!(matches!(result, Ok(Ok(()))));
        let query = format!(
            "SELECT pg_catalog.pg_backend_pid(),{} FROM pg_catalog.pg_stat_activity WHERE pid=pg_catalog.pg_backend_pid()",
            crate::native::iso("backend_start")
        );
        let row = tokio::time::timeout(CONTROL_TIMEOUT, actor.query_one(&tagged(&query), &[])).await.unwrap().unwrap();
        self.pid = row.get(0);
        self.backend_start = row.get(1);
        self.cancel = Some(actor.cancel_token());
        let (release, wait) = oneshot::channel();
        self.release = Some(release);
        let (ready, started) = oneshot::channel();
        let sql = format!("COPY {}.items(id) FROM STDIN", quote_ident(&self.schema));
        self.actor = Some(tokio::spawn(async move {
            let result = async {
                let sink = actor.copy_in::<_, std::io::Cursor<Vec<u8>>>(sql.as_str()).await?;
                tokio::pin!(sink);
                let data = b"1\n".repeat(COPY_ROWS as usize);
                assert_eq!(data.len() as u64, COPY_BYTES);
                sink.as_mut().send(std::io::Cursor::new(data)).await?;
                let _ = ready.send(());
                let _ = wait.await;
                sink.as_mut().finish().await
            }
            .await;
            (actor, result)
        }));
        tokio::time::timeout(CONTROL_TIMEOUT, started)
            .await
            .expect("owned COPY start deadline")
            .expect("owned COPY stream start");
        self.wait_for_copy().await;
    }

    async fn wait_for_copy(&mut self) {
        for _ in 0..MAX_OBSERVER_READS {
            self.admin_in_flight = true;
            let result=tokio::time::timeout(CONTROL_TIMEOUT,self.admin.as_ref().unwrap().query_opt(&tagged("SELECT bytes_processed,tuples_processed FROM pg_catalog.pg_stat_progress_copy WHERE pid=$1 AND datid=(SELECT oid FROM pg_catalog.pg_database WHERE datname=current_database())"),&[&self.pid])).await;
            let row = result.expect("owned COPY observer deadline").expect("owned COPY observer reply");
            self.admin_in_flight = false;
            if row.is_some_and(|row| row.get::<_, i64>(0) > 0 && row.get::<_, i64>(1) > 0) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        panic!("finite owned COPY input must reach native positive progress before observation");
    }

    async fn settle_actor(&mut self) -> Option<std::result::Result<u64, tokio_postgres::Error>> {
        let task = self.actor.as_mut()?;
        let Ok(result) = tokio::time::timeout(CONTROL_TIMEOUT, task).await else { return None };
        self.actor.take();
        let Ok((client, result)) = result else { return None };
        drop(client);
        Some(result)
    }

    pub(super) async fn release_and_complete(&mut self) {
        if let Some(release) = self.release.take() {
            let _ = release.send(());
        }
        assert_eq!(self.settle_actor().await.expect("owned COPY completion").unwrap(), COPY_ROWS);
        assert!(self.wait_for_actor_exit().await, "completed owned COPY connection must exit");
    }

    async fn wait_for_actor_exit(&mut self) -> bool {
        if self.pid == 0 {
            return true;
        }
        for _ in 0..MAX_OBSERVER_READS {
            if self.admin_in_flight {
                return false;
            }
            self.admin_in_flight = true;
            let query = format!(
                "SELECT NOT EXISTS(SELECT 1 FROM pg_catalog.pg_stat_activity WHERE pid=$1 AND {}=$2)",
                crate::native::iso("backend_start")
            );
            let result = tokio::time::timeout(
                CONTROL_TIMEOUT,
                self.admin.as_ref().unwrap().query_one(&tagged(&query), &[&self.pid, &self.backend_start]),
            )
            .await;
            let Ok(Ok(row)) = result else { return false };
            self.admin_in_flight = false;
            if row.get::<_, bool>(0) {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        false
    }

    async fn cleanup(&mut self) -> bool {
        if let Some(release) = self.release.take() {
            let _ = release.send(());
        }
        let mut settled = true;
        if self.actor.is_some() && self.settle_actor().await.is_none() {
            if let Some(cancel) = self.cancel.take() {
                settled &=
                    matches!(tokio::time::timeout(CONTROL_TIMEOUT, self.connector.cancel(&cancel)).await, Ok(Ok(())));
            }
            if self.settle_actor().await.is_none() {
                if let Some(task) = self.actor.take() {
                    task.abort();
                    let _ = task.await;
                }
                settled = false;
            }
        }
        settled &= self.wait_for_actor_exit().await;
        if !settled || self.admin_in_flight {
            self.admin.take();
            return false;
        }
        if self.schema_created {
            let sql = tagged(&format!("DROP SCHEMA IF EXISTS {} CASCADE", quote_ident(&self.schema)));
            settled &= matches!(
                tokio::time::timeout(CONTROL_TIMEOUT, self.admin.as_ref().unwrap().batch_execute(&sql)).await,
                Ok(Ok(()))
            );
        }
        self.admin.take();
        settled
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if let Some(actor) = &self.actor {
            actor.abort();
        }
    }
}
