use crate::{
    activity::page_targets::PageWaitCapture,
    activity::{ActivityReader, ActivityScope, IdentityDetail, ReadOptions},
    conn::{MsClient, Pool, SessionKind, map_error},
    index_operations,
    read::{Parameter, ReadError, ReadSession},
    test_support::config,
    value::Rec,
};
use culprit_core::{Error, ErrorCode, Result, telemetry::*};
use futures_util::{FutureExt, future::BoxFuture};
use std::{collections::BTreeMap, panic::AssertUnwindSafe, sync::Arc};

const MAX_OWNED_DATABASE_INDEX_ROWS: usize = 256;
const MEASURED_OBJECTS: [&str; 3] = ["page_fixture_hot", "page_fixture_split", "page_fixture_select"];

pub(super) struct Fixture {
    name: String,
    pub(super) port: u16,
    password: String,
    pub(super) pool: Option<Arc<Pool>>,
    pub(super) workers: Vec<Option<tokio::task::JoinHandle<Result<super::workload::CompletedWorker>>>>,
    admin: MsClient,
    owned_host: bool,
}
impl Fixture {
    async fn open() -> Result<Self> {
        let port =
            std::env::var("CULPRIT_MSSQL_SOURCE_TEST_PORT").expect("owned loopback port").parse::<u16>().unwrap();
        let password = std::env::var("CULPRIT_MSSQL_SOURCE_TEST_PASSWORD").expect("owned database password");
        let name = format!("culprit_page_contention_{}_{}", std::process::id(), chrono::Utc::now().timestamp_micros());
        let admin =
            crate::test_support::connect(&config(port, "master", &password, "page-fixture-admin"), SessionKind::User)
                .await?;
        Ok(Self { name, port, password, pool: None, workers: Vec::new(), admin, owned_host: false })
    }
    async fn initialize(&mut self) -> Result<()> {
        let rows = self
            .admin
            .simple_query("SELECT /* culprit */ CONVERT(nvarchar(128),SERVERPROPERTY('MachineName')) AS machine")
            .await
            .map_err(map_error)?
            .into_first_result()
            .await
            .map_err(map_error)?;
        let host = rows.first().map(Rec::from_row).ok_or_else(|| invalid("missing owned fixture identity"))?;
        let machine = crate::read::text(&host, "machine")?;
        if !machine
            .strip_prefix("culprit-smoke-mssql-")
            .is_some_and(|nonce| nonce.len() == 24 && nonce.bytes().all(|byte| byte.is_ascii_hexdigit()))
        {
            return Err(invalid("only an owned nonce-labelled smoke fixture may create page workloads"));
        }
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
        Ok(())
    }
    pub async fn client(&self, application: &str) -> Result<MsClient> {
        crate::test_support::connect(&config(self.port, &self.name, &self.password, application), SessionKind::User)
            .await
    }
    async fn execute(&mut self, sql: &str) -> Result<()> {
        execute(&mut self.admin, sql).await
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
                scope: ActivityScope::CurrentDatabase,
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
        self.pool.take();
        // This targets only the nonce database even if a worker, observer or test assertion failed midway.
        let result = if self.owned_host {
            self.execute(&format!("IF DB_ID(N'{}') IS NOT NULL BEGIN ALTER DATABASE [{}] SET SINGLE_USER WITH ROLLBACK IMMEDIATE; DROP DATABASE [{}]; END",self.name,self.name,self.name)).await
        } else {
            Ok(())
        };
        for task in self.workers.drain(..).flatten() {
            task.abort();
            let _ = task.await;
        }
        result
    }
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
    match outcome {
        Err(panic) => {
            if let Err(error) = cleanup {
                eprintln!("owned page fixture cleanup failed: {error}");
            }
            std::panic::resume_unwind(panic)
        }
        Ok(result) => {
            cleanup.expect("nonce database and native workers removed");
            result.expect("owned page qualification");
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
