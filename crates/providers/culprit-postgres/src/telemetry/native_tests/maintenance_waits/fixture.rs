use super::*;
use crate::telemetry::native_tests::case;
use crate::{
    connect::Connector,
    rows::{quote_ident, tagged},
};
use culprit_collector::capture::CollectionCancellation;
use culprit_core::ConnectionSpec;
use futures_util::{FutureExt, future::LocalBoxFuture};
use std::{panic::AssertUnwindSafe, time::Duration};
use tokio::task::JoinHandle;
use tokio_postgres::{CancelToken, Client};

const CASE_TIMEOUT: Duration = Duration::from_secs(25);
const CONTROL_TIMEOUT: Duration = Duration::from_secs(3);
type BuildResult = (Client, std::result::Result<(), tokio_postgres::Error>);

pub(super) struct Fixture {
    pub specification: ConnectionSpec,
    pub cancellation: CollectionCancellation,
    pub builder_pid: i32,
    pub holder_pid: i32,
    connector: Connector,
    schema: String,
    admin: Option<Client>,
    admin_in_flight: bool,
    holder: Option<Client>,
    build: Option<JoinHandle<BuildResult>>,
    cancel: Option<CancelToken>,
    idle_builder: Option<Client>,
    schema_created: bool,
}

impl Fixture {
    pub(super) async fn run(case: &str, body: for<'a> fn(&'a mut Self) -> LocalBoxFuture<'a, ()>) {
        let mut fixture = Self::open(case).await;
        let cancellation = fixture.cancellation.clone();
        let completed =
            case::finish(CASE_TIMEOUT, &cancellation, AssertUnwindSafe(body(&mut fixture)).catch_unwind()).await;
        let settled = fixture.cleanup().await;
        eprintln!("owned maintenance fixture cleanup_confirmed={settled}; source cost remains unqualified");
        if let Err(panic) = completed.value {
            std::panic::resume_unwind(panic);
        }
        assert!(
            !completed.deadline_reached,
            "owned maintenance fixture exceeded its absolute deadline; cleanup_confirmed={settled}"
        );
        assert!(settled, "owned maintenance fixture cleanup must be confirmed");
    }

    async fn open(case: &str) -> Self {
        assert!(matches!(case, "waiting" | "released"));
        let database = std::env::var("CULPRIT_PREVIEW_PG_DATABASE").unwrap();
        let specification = owned_fixture::specification(&database, CollectionBudget::default()).await;
        let connector = Connector::new(&specification).unwrap();
        let admin =
            tokio::time::timeout(CONTROL_TIMEOUT, connector.connect_unconfigured()).await.unwrap().unwrap().client;
        Self {
            specification,
            cancellation: CollectionCancellation::default(),
            connector,
            schema: format!("culprit_maintenance_{}_{}", owned_fixture::owner(), case),
            builder_pid: 0,
            holder_pid: 0,
            admin: Some(admin),
            admin_in_flight: false,
            holder: None,
            build: None,
            cancel: None,
            idle_builder: None,
            schema_created: false,
        }
    }

    async fn execute(&mut self, sql: &str) {
        assert!(!self.admin_in_flight);
        self.admin_in_flight = true;
        let result =
            tokio::time::timeout(CONTROL_TIMEOUT, self.admin.as_ref().unwrap().batch_execute(&tagged(sql))).await;
        assert!(matches!(result, Ok(Ok(()))), "owned fixture control SQL did not acknowledge completion");
        self.admin_in_flight = false;
    }

    pub(super) async fn prepare(&mut self) {
        self.execute("SET statement_timeout='1s';SET lock_timeout='1s'").await;
        self.schema_created = true; // Retain cleanup ownership before the first mutation can be dispatched.
        self.execute(&format!("CREATE SCHEMA {}", quote_ident(&self.schema))).await;
        self.execute(&format!(
            "CREATE TABLE {}.items(id integer PRIMARY KEY,value integer NOT NULL);INSERT INTO {}.items VALUES(1,0)",
            quote_ident(&self.schema),
            quote_ident(&self.schema)
        ))
        .await;
        let holder =
            tokio::time::timeout(CONTROL_TIMEOUT, self.connector.connect_unconfigured()).await.unwrap().unwrap().client;
        self.holder = Some(holder);
        let holder = self.holder.as_ref().unwrap();
        let result = tokio::time::timeout(
            CONTROL_TIMEOUT,
            holder.batch_execute(&tagged(
                "SET statement_timeout='1s';SET lock_timeout='1s';SET idle_in_transaction_session_timeout='35s'",
            )),
        )
        .await;
        assert!(matches!(result, Ok(Ok(()))));
        self.holder_pid =
            tokio::time::timeout(CONTROL_TIMEOUT, holder.query_one(&tagged("SELECT pg_backend_pid()"), &[]))
                .await
                .unwrap()
                .unwrap()
                .get(0);
        let result = tokio::time::timeout(
            CONTROL_TIMEOUT,
            holder.batch_execute(&tagged(&format!(
                "BEGIN;UPDATE {}.items SET value=value+1 WHERE id=1",
                quote_ident(&self.schema)
            ))),
        )
        .await;
        assert!(matches!(result, Ok(Ok(()))));
        let builder =
            tokio::time::timeout(CONTROL_TIMEOUT, self.connector.connect_unconfigured()).await.unwrap().unwrap().client;
        let result = tokio::time::timeout(
            CONTROL_TIMEOUT,
            builder.batch_execute(&tagged(
                "SET statement_timeout='30s';SET lock_timeout='0';SET application_name='fixture-concurrent-index'",
            )),
        )
        .await;
        assert!(matches!(result, Ok(Ok(()))));
        self.builder_pid =
            tokio::time::timeout(CONTROL_TIMEOUT, builder.query_one(&tagged("SELECT pg_backend_pid()"), &[]))
                .await
                .unwrap()
                .unwrap()
                .get(0);
        self.cancel = Some(builder.cancel_token());
        let sql = format!("CREATE INDEX CONCURRENTLY maintenance_value ON {}.items(value)", quote_ident(&self.schema));
        self.build = Some(tokio::spawn(async move {
            let result = builder.batch_execute(&sql).await;
            (builder, result)
        }));
    }

    pub(super) async fn wait_for_writer(&mut self) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
        loop {
            assert!(!self.admin_in_flight);
            self.admin_in_flight = true;
            let result = tokio::time::timeout(CONTROL_TIMEOUT, self.admin.as_ref().unwrap().query_opt(
                &tagged("SELECT phase,current_locker_pid FROM pg_catalog.pg_stat_progress_create_index WHERE pid=$1 AND datid=(SELECT oid FROM pg_catalog.pg_database WHERE datname=current_database())"),
                &[&self.builder_pid],
            )).await;
            let row = result.expect("owned fixture phase deadline").expect("owned fixture phase observation");
            self.admin_in_flight = false;
            if row.is_some_and(|row| {
                row.get::<_, String>(0) == "waiting for writers before build"
                    && row.get::<_, i64>(1) == i64::from(self.holder_pid)
            }) {
                return;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "owned UPDATE must hold the concurrent index in the documented native wait"
            );
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }

    async fn release(&mut self) -> bool {
        let Some(holder) = self.holder.take() else { return true };
        matches!(tokio::time::timeout(CONTROL_TIMEOUT, holder.batch_execute(&tagged("ROLLBACK"))).await, Ok(Ok(())))
    }

    async fn settle_build(&mut self) -> Option<bool> {
        let build = self.build.as_mut()?;
        let Ok(result) = tokio::time::timeout(CONTROL_TIMEOUT, build).await else { return None };
        self.build.take();
        let Ok((client, result)) = result else { return None };
        self.idle_builder = Some(client);
        Some(result.is_ok())
    }

    pub(super) async fn release_and_complete(&mut self) {
        assert!(self.release().await);
        assert_eq!(
            self.settle_build().await,
            Some(true),
            "only the owned test observes acknowledged CREATE completion"
        );
    }

    async fn cleanup(&mut self) -> bool {
        let mut settled = self.release().await;
        if self.build.is_some() && self.settle_build().await.is_none() {
            if let Some(cancel) = self.cancel.take() {
                settled &=
                    matches!(tokio::time::timeout(CONTROL_TIMEOUT, self.connector.cancel(&cancel)).await, Ok(Ok(())));
            }
            settled &= self.settle_build().await.is_some();
        }
        if let Some(build) = self.build.take() {
            build.abort();
            let _ = build.await;
            settled = false;
        }
        self.idle_builder.take();
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
        settled
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if let Some(build) = &self.build {
            build.abort();
        }
        // Dropping clients closes fixture-only sessions; a bounded native statement timeout remains the final guard.
    }
}
