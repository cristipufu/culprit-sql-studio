use crate::{
    activity::page_targets::PageWaitCapture,
    activity::{ActivityReader, IdentityDetail, ReadOptions},
    conn::{MsClient, Pool, SessionKind, map_error},
    index_operations,
    read::{Parameter, ReadError, ReadSession},
    test_support::config,
};
use culprit_core::{Error, ErrorCode, Result, telemetry::*};
use futures_util::{FutureExt, future::BoxFuture};
use std::{collections::BTreeMap, future::Future, panic::AssertUnwindSafe, sync::Arc, time::Duration};

const MAX_OWNED_DATABASE_INDEX_ROWS: usize = 256;
const WORKER_CLEANUP_TIMEOUT: Duration = Duration::from_secs(2);
const HOLDER_GATE_CONTROL_TIMEOUT: Duration = Duration::from_secs(2);
const DATABASE_CLEANUP_TIMEOUT: Duration = Duration::from_secs(8);
const MEASURED_OBJECTS: [&str; 3] = ["page_fixture_hot", "page_fixture_split", "page_fixture_select"];

struct HolderGate {
    client: Option<MsClient>,
    transaction_possible: bool,
}

pub(super) struct Fixture {
    name: String,
    pub(super) port: u16,
    password: String,
    pub(super) pool: Option<Arc<Pool>>,
    pub(super) workers: Vec<Option<tokio::task::JoinHandle<Result<super::workload::CompletedWorker>>>>,
    pub(super) setup_elapsed_ms: f64,
    setup_started: std::time::Instant,
    admin: Option<MsClient>,
    holder_gate: Option<HolderGate>,
    owned_host: bool,
}
impl Fixture {
    async fn open() -> Result<Self> {
        let setup_started = std::time::Instant::now();
        let port =
            std::env::var("CULPRIT_MSSQL_SOURCE_TEST_PORT").expect("owned loopback port").parse::<u16>().unwrap();
        let password = std::env::var("CULPRIT_MSSQL_SOURCE_TEST_PASSWORD").expect("owned database password");
        let name = format!("culprit_page_contention_{}_{}", std::process::id(), chrono::Utc::now().timestamp_micros());
        let admin =
            crate::test_support::connect(&config(port, "master", &password, "page-fixture-admin"), SessionKind::User)
                .await?;
        Ok(Self {
            name,
            port,
            password,
            pool: None,
            workers: Vec::new(),
            setup_elapsed_ms: 0.0,
            setup_started,
            admin: Some(admin),
            holder_gate: None,
            owned_host: false,
        })
    }
    async fn initialize(&mut self) -> Result<()> {
        crate::test_support::verify_owned_source(
            self.admin.as_mut().ok_or_else(|| invalid("owned fixture admin connection was retired"))?,
        )
        .await?;
        self.owned_host = true;
        self.execute(&format!(
            "CREATE DATABASE [{}]; ALTER DATABASE [{}] SET RECOVERY SIMPLE;
            ALTER DATABASE [{}] MODIFY FILE(NAME=N'{}',MAXSIZE=64MB,FILEGROWTH=16MB);
            ALTER DATABASE [{}] MODIFY FILE(NAME=N'{}_log',MAXSIZE=128MB,FILEGROWTH=16MB);
            ALTER DATABASE [{}] SET DELAYED_DURABILITY=FORCED;",
            self.name, self.name, self.name, self.name, self.name, self.name, self.name
        ))
        .await?;
        let pool = Arc::new(Pool::new(config(self.port, &self.name, &self.password, "culprit"), None));
        self.pool = Some(pool);
        let mut setup = self.client("page-fixture-setup").await?;
        // Delayed durability is an owned-fixture control to isolate page synchronization from log flush waits.
        // It is never a collector action or production recommendation.
        for statement in [
            "CREATE TABLE dbo.page_fixture_hot(id bigint IDENTITY NOT NULL CONSTRAINT PK_page_fixture_hot PRIMARY KEY CLUSTERED,marker tinyint NOT NULL,payload binary(16) NOT NULL DEFAULT 0x01)",
            "CREATE TABLE dbo.page_fixture_split(id bigint IDENTITY NOT NULL CONSTRAINT PK_page_fixture_split PRIMARY KEY CLUSTERED,marker tinyint NOT NULL,payload binary(16) NOT NULL DEFAULT 0x01)",
            "CREATE TABLE dbo.page_fixture_select(id int NOT NULL PRIMARY KEY); WITH n AS(SELECT v FROM(VALUES(0),(1),(2),(3),(4),(5),(6),(7),(8),(9))x(v)) INSERT dbo.page_fixture_select SELECT a.v*10+b.v FROM n a CROSS JOIN n b",
            "INSERT dbo.page_fixture_hot(marker) VALUES(0); INSERT dbo.page_fixture_split(marker) VALUES(0); SELECT SUM(id) FROM dbo.page_fixture_select; CHECKPOINT",
        ] {
            execute(&mut setup, statement).await?;
        }
        self.setup_elapsed_ms = self.setup_started.elapsed().as_secs_f64() * 1000.0;
        Ok(())
    }
    pub async fn client(&self, application: &str) -> Result<MsClient> {
        crate::test_support::connect(&config(self.port, &self.name, &self.password, application), SessionKind::User)
            .await
    }
    async fn execute(&mut self, sql: &str) -> Result<()> {
        let mut admin = self.admin.take().ok_or_else(|| invalid("owned fixture admin connection was retired"))?;
        let result = execute(&mut admin, sql).await;
        // Only a fully drained successful batch permits reuse. Failure or cancellation retires this actor.
        if result.is_ok() {
            self.admin = Some(admin);
        }
        result
    }
    /// The retained owner exists before connection or transaction work can be cancelled. Only this nonce
    /// fixture's seed table is locked; no page latch, task pointer or native holder relationship is imposed.
    pub async fn start_holder_gate(&mut self) -> Result<()> {
        if !self.owned_host || self.pool.is_none() || self.holder_gate.is_some() || !self.workers.is_empty() {
            return Err(invalid("owned holder gate requires an initialized idle fixture"));
        }
        self.holder_gate = Some(HolderGate { client: None, transaction_possible: false });
        let client = tokio::time::timeout(HOLDER_GATE_CONTROL_TIMEOUT, self.client("page-fixture-holder-gate"))
            .await
            .map_err(|_| invalid("owned holder gate connection exceeded its bound"))??;
        self.holder_gate.as_mut().ok_or_else(|| invalid("owned holder gate was retired"))?.client = Some(client);
        self.holder_gate_command(
            "SET LOCK_TIMEOUT 1000; BEGIN TRANSACTION; SELECT TOP(1) id FROM dbo.page_fixture_select WITH(TABLOCKX,HOLDLOCK);",
        )
        .await
    }

    pub(super) fn holder_gate_active(&self) -> bool {
        self.holder_gate.as_ref().is_some_and(|gate| gate.transaction_possible && gate.client.is_some())
    }

    pub async fn release_holder_gate(&mut self) -> Result<()> {
        if !self.holder_gate_active() {
            return Err(invalid("owned holder gate is absent or its control response is unconfirmed"));
        }
        self.holder_gate_command("IF @@TRANCOUNT>0 ROLLBACK TRANSACTION;").await?;
        self.holder_gate.take();
        Ok(())
    }

    async fn holder_gate_command(&mut self, sql: &str) -> Result<()> {
        let gate = self.holder_gate.as_mut().ok_or_else(|| invalid("owned holder gate was retired"))?;
        let mut client = gate.client.take().ok_or_else(|| invalid("owned holder gate control is unconfirmed"))?;
        gate.transaction_possible = true;
        // The local actor is dropped on timeout, error, panic or cancellation. An undrained stream is never
        // returned to the retained owner, even when BEGIN succeeded before the control response was lost.
        let result = bounded_holder_gate_control(execute(&mut client, sql)).await;
        if result.is_ok() {
            self.holder_gate.as_mut().ok_or_else(|| invalid("owned holder gate was retired"))?.client = Some(client);
        }
        result
    }

    async fn close_holder_gate(&mut self) -> Result<()> {
        let Some(gate) = self.holder_gate.as_ref() else { return Ok(()) };
        if !gate.transaction_possible {
            self.holder_gate.take();
            return Ok(());
        }
        if gate.client.is_none() {
            self.holder_gate.take();
            return Err(invalid("owned holder gate actor was retired; transaction cleanup is unconfirmed"));
        }
        let result = self.release_holder_gate().await;
        // Release failure must still close the actor before worker settlement or database cleanup begins.
        self.holder_gate.take();
        result
    }

    pub async fn reset(&self) -> Result<()> {
        let mut client = self.client("page-fixture-reset").await?;
        execute(&mut client,"TRUNCATE TABLE dbo.page_fixture_hot; TRUNCATE TABLE dbo.page_fixture_split; INSERT dbo.page_fixture_hot(marker) VALUES(0); INSERT dbo.page_fixture_split(marker) VALUES(0); CHECKPOINT").await
    }
    pub async fn inspect(&self) -> Result<(PageWaitCapture, ReadWorkMetrics)> {
        let mut reader = ActivityReader::open(
            self.pool.as_ref().unwrap().clone(),
            ReadOptions {
                budget: CollectionBudget {
                    max_queries: 24,
                    statement_timeout_ms: 1000,
                    total_timeout_ms: 3000,
                    max_sql_bytes: 16 * 1024,
                    ..Default::default()
                },
                identity: IdentityDetail::Endpoint,
            },
            "127.0.0.1".into(),
            self.port,
        )?;
        let captured = reader.capture_page_wait_targets().await;
        let shutdown = reader.close().await;
        let metrics = reader.read_metrics();
        if captured.is_err() || shutdown != SourceShutdown::Quiescent {
            eprintln!(
                "owned page capture failure: error={:?},shutdown={shutdown:?},metrics={}",
                captured.as_ref().err(),
                serde_json::to_string(&metrics).unwrap_or_else(|error| format!("metrics encoding failed: {error}"))
            );
        }
        if shutdown != SourceShutdown::Quiescent {
            return Err(CollectionStop::CancellationUnconfirmed.error());
        }
        Ok((captured?, metrics.ok_or_else(|| Error::invalid("missing owned native read metrics"))?))
    }
    pub async fn indexes(&self) -> Result<BTreeMap<String, MssqlIndexOperations>> {
        let mut reader = ReadSession::new(
            self.pool.as_ref().unwrap().clone(),
            CollectionBudget {
                max_queries: 4,
                statement_timeout_ms: 1000,
                total_timeout_ms: 3000,
                ..Default::default()
            },
        );
        let result = async {
            let layout = index_operations::Layout::read(&mut reader).await?;
            let rows = reader
                .query(
                    &index_operations::select_sql(&layout),
                    vec![Parameter::Integer((MAX_OWNED_DATABASE_INDEX_ROWS + 1) as i64)],
                    MAX_OWNED_DATABASE_INDEX_ROWS + 1,
                )
                .await
                .map_err(ReadError::into_error)?;
            if rows.len() > MAX_OWNED_DATABASE_INDEX_ROWS {
                return Err(invalid("owned native index population exceeded its explicit bound"));
            }
            let mut measured = BTreeMap::new();
            for row in &rows {
                let native = index_operations::decode(row, &layout)?;
                if native.schema.as_deref() != Some("dbo") {
                    continue;
                }
                let Some(name) = native.object.as_deref().filter(|name| MEASURED_OBJECTS.contains(name)) else {
                    continue;
                };
                if measured.insert(name.to_owned(), native).is_some() {
                    return Err(invalid("owned fixture unexpectedly has multiple native index rows per object"));
                }
            }
            if measured.len() != MEASURED_OBJECTS.len() {
                return Err(invalid("owned native index population omitted a required fixture object"));
            }
            Ok(measured)
        }
        .await;
        let shutdown = reader.close().await;
        if shutdown != SourceShutdown::Quiescent {
            Err(CollectionStop::CancellationUnconfirmed.error())
        } else {
            result
        }
    }
    pub async fn qualify_index_point(&self, point: &PageIndexOperations) -> Result<()> {
        let PageIndexOperations::Observed { counters, partition_id: Some(partition_id), .. } = point else {
            return Err(invalid("native cost proof needs the captured checked partition identity"));
        };
        let mut reader = ReadSession::new(
            self.pool.as_ref().unwrap().clone(),
            CollectionBudget {
                max_queries: 1,
                statement_timeout_ms: 1000,
                total_timeout_ms: 3000,
                ..Default::default()
            },
        );
        let layout = index_operations::Layout::read(&mut reader).await;
        let layout = reader.finish(layout).await?;
        let identity = &counters.identity;
        let target = index_operations::targeted::Target {
            database_id: identity.database_id,
            object_id: identity.object_id,
            index_id: identity.index_id,
            partition_number: identity.partition_number,
            partition_id: partition_id.parse().map_err(|_| invalid("invalid captured native partition id"))?,
        };
        if !target.is_valid() {
            return Err(invalid("owned index cost proof cannot use wildcard inputs"));
        }
        let mut qualification = self.client("page-fixture-index-cost").await?;
        let plans = tokio::time::timeout(
            std::time::Duration::from_secs(3),
            crate::test_support::assert_no_execution_grant(
                &mut qualification,
                &index_operations::targeted::query(&layout, 1),
                target.parameters().into(),
            ),
        )
        .await
        .map_err(|_| invalid("owned targeted index cost read exceeded its bound"))?;
        for plan in plans {
            culprit_core::xml::read(&plan, |document| {
                let document = document.unwrap();
                let mut partition_seeks = 0;
                for operation in document.descendants().filter(|node| node.has_tag_name("RelOp")) {
                    let rowsets = operation
                        .children()
                        .filter(|node| node.has_tag_name("IndexScan"))
                        .flat_map(|node| node.children())
                        .any(|node| node.has_tag_name("Object") && node.attribute("Table") == Some("[sysrowsets]"));
                    if rowsets {
                        partition_seeks += 1;
                        assert!(
                            matches!(operation.attribute("PhysicalOp"), Some("Clustered Index Seek" | "Index Seek")),
                            "targeted metadata must not scan all database rowsets: {plan}"
                        );
                        assert!(
                            operation
                                .descendants()
                                .filter(|node| node.has_tag_name("SeekPredicates"))
                                .flat_map(|node| node.descendants())
                                .any(|node| node.has_tag_name("ColumnReference")
                                    && node.attribute("Column") == Some("@P5")),
                            "native partition identity must drive the metadata seek: {plan}"
                        );
                    }
                }
                assert!(partition_seeks > 0, "the owned plan must expose native partition-key access: {plan}");
            });
            eprintln!("owned targeted index actual plan: {plan}");
        }
        Ok(())
    }
    async fn close(&mut self) -> Result<()> {
        let gate = self.close_holder_gate().await;
        let workers = self.stop_workers().await;
        self.pool.take();
        let database = self.drop_owned_database().await;
        let mut first_error = None;
        for result in [gate, workers, database] {
            if let Err(error) = result {
                if first_error.is_some() {
                    eprintln!("owned page fixture cleanup also failed: {error}");
                } else {
                    first_error = Some(error);
                }
            }
        }
        first_error.map_or(Ok(()), Err)
    }
    async fn stop_workers(&mut self) -> Result<()> {
        for task in self.workers.iter().flatten() {
            task.abort();
        }
        let deadline = tokio::time::Instant::now() + WORKER_CLEANUP_TIMEOUT;
        let mut failure = None;
        for slot in &mut self.workers {
            let Some(task) = slot.as_mut() else { continue };
            let result = tokio::time::timeout_at(deadline, task)
                .await
                .map_err(|_| invalid("owned worker cleanup timed out; settlement is unconfirmed"))?;
            // Pending handles stay in the fixture through every await, including cancellation/timeout.
            slot.take();
            let error = match result {
                Ok(Ok(_)) => None,
                Ok(Err(error)) => Some(error),
                Err(error) if error.is_cancelled() => None,
                Err(_) => Some(invalid("owned worker panicked during cleanup")),
            };
            if failure.is_none() {
                failure = error;
            }
        }
        self.workers.clear();
        failure.map_or(Ok(()), Err)
    }
    async fn drop_owned_database(&mut self) -> Result<()> {
        if !self.owned_host {
            self.admin.take();
            return Ok(());
        }
        let mut admin = self.admin.take().ok_or_else(|| {
            invalid("owned database cleanup is unconfirmed; its admin connection was already retired")
        })?;
        // Cleanup owns the admin outright. Timeout or a dropped cleanup future closes it; no later SQL may
        // reuse a connection whose ALTER/DROP response was not fully drained.
        let sql = format!(
            "IF DB_ID(N'{}') IS NOT NULL BEGIN ALTER DATABASE [{}] SET SINGLE_USER WITH ROLLBACK IMMEDIATE; DROP DATABASE [{}]; END",
            self.name, self.name, self.name
        );
        bounded_database_cleanup(async move { execute(&mut admin, &sql).await }).await
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        // Close the gate first, including cancellation during cleanup. Drop requests cancellation; only
        // acknowledged control, joined workers and exact database cleanup establish their own settlement.
        self.holder_gate.take();
        for task in self.workers.iter().flatten() {
            task.abort();
        }
    }
}

async fn bounded_holder_gate_control(control: impl Future<Output = Result<()>>) -> Result<()> {
    tokio::time::timeout(HOLDER_GATE_CONTROL_TIMEOUT, control)
        .await
        .map_err(|_| invalid("owned holder gate control timed out; its actor is retired and cleanup is unconfirmed"))?
}

async fn bounded_database_cleanup(cleanup: impl Future<Output = Result<()>>) -> Result<()> {
    tokio::time::timeout(DATABASE_CLEANUP_TIMEOUT, cleanup)
        .await
        .map_err(|_| invalid("owned database cleanup timed out; its admin was retired and cleanup is unconfirmed"))?
}

pub(super) async fn run(test: impl for<'a> FnOnce(&'a mut Fixture) -> BoxFuture<'a, Result<()>>) {
    let mut fixture = Fixture::open().await.expect("owned loopback fixture connection");
    let outcome = AssertUnwindSafe(async {
        fixture.initialize().await?;
        test(&mut fixture).await
    })
    .catch_unwind()
    .await;
    let cleanup = fixture.close().await;
    if let Err(error) = &cleanup {
        eprintln!("owned page fixture cleanup failed: {error}");
    }
    match outcome {
        Err(panic) => std::panic::resume_unwind(panic),
        Ok(result) => {
            // Preserve the original failure; cleanup errors above remain independently visible.
            result.expect("owned page qualification");
            cleanup.expect("nonce database and native workers removed");
        }
    }
}
pub(super) async fn execute(client: &mut MsClient, sql: &str) -> Result<()> {
    client.simple_query(sql).await.map_err(map_error)?.into_results().await.map_err(map_error)?;
    Ok(())
}
pub(super) fn invalid(reason: &str) -> Error {
    Error::new(ErrorCode::InvalidInput, reason)
}

#[cfg(test)]
mod cleanup_tests {
    use super::*;
    use std::future::pending;
    use tokio::sync::oneshot;

    struct Retired(Option<oneshot::Sender<()>>);
    impl Drop for Retired {
        fn drop(&mut self) {
            if let Some(retired) = self.0.take() {
                let _ = retired.send(());
            }
        }
    }

    fn unconnected_fixture() -> Fixture {
        Fixture {
            name: String::new(),
            port: 0,
            password: String::new(),
            pool: None,
            workers: Vec::new(),
            setup_elapsed_ms: 0.0,
            setup_started: std::time::Instant::now(),
            admin: None,
            holder_gate: None,
            owned_host: false,
        }
    }

    async fn pending_worker(fixture: &mut Fixture) -> oneshot::Receiver<()> {
        let (started, ready) = oneshot::channel();
        let (retired, finished) = oneshot::channel();
        fixture.workers.push(Some(tokio::spawn(async move {
            let _actor = Retired(Some(retired));
            started.send(()).unwrap();
            pending::<Result<super::super::workload::CompletedWorker>>().await
        })));
        ready.await.unwrap();
        finished
    }

    #[tokio::test(start_paused = true)]
    async fn close_aborts_and_joins_every_worker_without_native_connections() {
        let mut fixture = unconnected_fixture();
        let mut retired = Vec::new();
        for _ in 0..3 {
            retired.push(pending_worker(&mut fixture).await);
        }
        fixture.close().await.unwrap();
        assert!(fixture.workers.is_empty());
        for actor in retired {
            actor.await.expect("every worker's owned state was dropped before cleanup returned");
        }
    }

    #[tokio::test(start_paused = true)]
    async fn cancellation_of_normal_completion_keeps_the_pending_handle_owned_by_the_fixture() {
        let mut fixture = unconnected_fixture();
        let retired = pending_worker(&mut fixture).await;
        assert!(tokio::time::timeout(Duration::from_millis(1), fixture.finish_workload()).await.is_err());
        assert_eq!(fixture.workers.len(), 1);
        assert!(fixture.workers[0].is_some(), "dropping the waiter must not detach its popped worker");
        fixture.close().await.unwrap();
        retired.await.expect("cancelled completion still has an explicitly settled worker");
    }

    #[tokio::test(start_paused = true)]
    async fn dropping_the_fixture_requests_worker_cancellation_instead_of_silently_detaching_it() {
        let mut fixture = unconnected_fixture();
        let retired = pending_worker(&mut fixture).await;
        drop(fixture);
        tokio::time::timeout(WORKER_CLEANUP_TIMEOUT, retired)
            .await
            .expect("aborted worker must be polled for retirement")
            .expect("fixture Drop retires the worker's owned state");
    }

    #[tokio::test(start_paused = true)]
    async fn uncertain_holder_gate_cleanup_still_joins_every_owned_worker_and_never_reports_success() {
        let mut fixture = unconnected_fixture();
        fixture.holder_gate = Some(HolderGate { client: None, transaction_possible: true });
        let retired = pending_worker(&mut fixture).await;
        let error = fixture.close().await.unwrap_err();
        assert!(error.message.contains("unconfirmed"));
        assert!(fixture.holder_gate.is_none());
        assert!(fixture.workers.is_empty());
        retired.await.expect("an unconfirmed gate cannot prevent explicit worker settlement");
    }

    #[tokio::test(start_paused = true)]
    async fn interrupted_holder_gate_control_retires_its_owned_actor_without_an_acknowledged_transaction() {
        assert!(bounded_holder_gate_control(async { Ok(()) }).await.is_ok());
        for cancelled in [true, false] {
            let (actor, retired) = oneshot::channel();
            let control = bounded_holder_gate_control(async move {
                let _actor = Retired(Some(actor));
                pending::<Result<()>>().await
            });
            if cancelled {
                assert!(tokio::time::timeout(Duration::from_millis(1), control).await.is_err());
            } else {
                let error = control.await.unwrap_err();
                assert!(error.message.contains("unconfirmed"));
            }
            retired.await.expect("an interrupted gate control cannot retain a reusable actor");
        }
    }

    #[tokio::test(start_paused = true)]
    async fn timed_out_or_cancelled_cleanup_retires_its_owned_actor_without_claiming_success() {
        assert!(bounded_database_cleanup(async { Ok(()) }).await.is_ok());
        let failure =
            bounded_database_cleanup(async { Err(invalid("owned cleanup control failure")) }).await.unwrap_err();
        assert_eq!(failure.message, "owned cleanup control failure");
        for cancelled in [true, false] {
            let (actor, retired) = oneshot::channel();
            let cleanup = bounded_database_cleanup(async move {
                let _actor = Retired(Some(actor));
                pending::<Result<()>>().await
            });
            if cancelled {
                assert!(tokio::time::timeout(Duration::from_millis(1), cleanup).await.is_err());
            } else {
                let error = cleanup.await.unwrap_err();
                assert!(error.message.contains("unconfirmed"));
            }
            retired.await.expect("cleanup never returns an uncertain actor for another statement");
        }
    }
}
