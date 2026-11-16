use super::*;
use crate::{
    conn::MsClient,
    read::{integer, text},
    value::Rec,
};

const ACTOR_WORK: &str = "SET NOCOUNT ON; CREATE TABLE #culprit_cost_task(payload varchar(2000) NOT NULL); WITH n AS(SELECT v FROM(VALUES(0),(1),(2),(3),(4),(5),(6),(7))d(v)) INSERT #culprit_cost_task SELECT REPLICATE('x',2000) FROM n a CROSS JOIN n b; WAITFOR DELAY '00:02:00'; DROP TABLE #culprit_cost_task;";

pub(super) struct CostFixture {
    pub(super) pool: Arc<Pool>,
    pub(super) port: u16,
    pub(super) password: String,
    pub(super) database: String,
    pub(super) actor_ids: BTreeSet<String>,
    pub(super) setup_elapsed_ms: f64,
    pub(super) readiness_queries: u32,
    admin: MsClient,
    actors: Vec<tokio::task::JoinHandle<culprit_core::Result<()>>>,
    control_in_flight: bool,
}
impl CostFixture {
    pub(super) async fn open(actors: usize) -> culprit_core::Result<Self> {
        let started = Instant::now();
        let port = std::env::var("CULPRIT_MSSQL_SOURCE_TEST_PORT").unwrap().parse().unwrap();
        let password = std::env::var("CULPRIT_MSSQL_SOURCE_TEST_PASSWORD").unwrap();
        let database =
            format!("culprit_cost_{}_{}_{}", std::process::id(), chrono::Utc::now().timestamp_micros(), actors);
        let mut admin =
            crate::test_support::connect(&config(port, "master", &password, "cost-fixture-admin"), SessionKind::User)
                .await?;
        let rows = admin
            .simple_query(
                "SELECT /* culprit fixture guard */ CONVERT(nvarchar(128),SERVERPROPERTY('MachineName')) AS machine",
            )
            .await
            .map_err(map_error)?
            .into_first_result()
            .await
            .map_err(map_error)?;
        let machine = rows.first().map(Rec::from_row).ok_or_else(crate::read::invalid_data)?;
        let machine = text(&machine, "machine")?;
        assert!(
            machine
                .strip_prefix("culprit-smoke-mssql-")
                .is_some_and(|nonce| nonce.len() == 24 && nonce.bytes().all(|byte| byte.is_ascii_hexdigit())),
            "native cost workloads require the actual owned smoke hostname"
        );
        Ok(Self {
            pool: Arc::new(Pool::new(config(port, &database, &password, "culprit-cost-source"), None)),
            port,
            password,
            database,
            actor_ids: Default::default(),
            admin,
            actors: vec![],
            setup_elapsed_ms: started.elapsed().as_secs_f64() * 1000.0,
            readiness_queries: 0,
            control_in_flight: false,
        })
    }
    pub(super) async fn initialize(&mut self) -> culprit_core::Result<()> {
        let started = Instant::now();
        let create = format!("/* culprit fixture setup */ CREATE DATABASE [{}]", self.database);
        self.control_batch(&create).await?;
        // USE binds its database when this second batch compiles. Creation must already have an EOF
        // acknowledgement; putting both operations in one batch does not establish that ordering.
        let setup = format!(
            "/* culprit fixture setup */ ALTER DATABASE [{database}] SET RECOVERY SIMPLE; ALTER DATABASE [{database}] MODIFY FILE(NAME=N'{database}',MAXSIZE=64MB,FILEGROWTH=16MB); ALTER DATABASE [{database}] MODIFY FILE(NAME=N'{database}_log',MAXSIZE=64MB,FILEGROWTH=16MB); USE [{database}]; CREATE TABLE dbo.cost_load(id bigint IDENTITY PRIMARY KEY,payload binary(16) NOT NULL DEFAULT 0x01)",
            database = self.database,
        );
        self.control_batch(&setup).await?;
        self.setup_elapsed_ms += started.elapsed().as_secs_f64() * 1000.0;
        Ok(())
    }
    async fn control_batch(&mut self, sql: &str) -> culprit_core::Result<()> {
        self.control_in_flight = true;
        self.admin.simple_query(sql).await.map_err(map_error)?.into_results().await.map_err(map_error)?;
        self.control_in_flight = false;
        Ok(())
    }
    pub(super) async fn client(&self, application: &str) -> culprit_core::Result<MsClient> {
        crate::test_support::connect(&config(self.port, &self.database, &self.password, application), SessionKind::User)
            .await
    }
    pub(super) async fn start_actors(&mut self, count: usize) -> culprit_core::Result<()> {
        for _ in 0..count {
            let mut client = self.client("cost-fixture-allocator").await?;
            self.actor_ids.insert(session_id(&mut client).await);
            self.actors.push(tokio::spawn(async move {
                client.simple_query(ACTOR_WORK).await.map_err(map_error)?.into_results().await.map_err(map_error)?;
                Ok(())
            }));
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            self.readiness_queries += 1;
            let mut query = tiberius::Query::new(
                "SELECT /* culprit fixture readiness */ CONVERT(bigint,COUNT_BIG(*)) AS ready FROM sys.dm_exec_requests r JOIN sys.dm_exec_sessions s ON s.session_id=r.session_id WHERE r.database_id=DB_ID(@P1) AND s.program_name=N'cost-fixture-allocator' AND r.command=N'WAITFOR'",
            );
            query.bind(self.database.as_str());
            self.control_in_flight = true;
            let row = query
                .query(&mut self.admin)
                .await
                .map_err(map_error)?
                .into_row()
                .await
                .map_err(map_error)?
                .ok_or_else(crate::read::invalid_data)?;
            self.control_in_flight = false;
            if integer(&Rec::from_row(&row), "ready")? as usize == count {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(crate::read::invalid_data());
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
    pub(super) async fn close(&mut self) -> culprit_core::Result<()> {
        for actor in &self.actors {
            actor.abort();
        }
        while let Some(actor) = self.actors.pop() {
            let _ = actor.await;
        }
        if self.control_in_flight {
            // An unacknowledged control batch cannot be followed on this socket. Preserve incomplete
            // scenario settlement; the supervisor's owned-container cleanup is separate containment.
            return Err(CollectionStop::CancellationUnconfirmed.error());
        }
        let sql = format!(
            "USE master; IF DB_ID(N'{}') IS NOT NULL BEGIN ALTER DATABASE [{}] SET SINGLE_USER WITH ROLLBACK IMMEDIATE; DROP DATABASE [{}]; END",
            self.database, self.database, self.database
        );
        self.control_batch(&sql).await
    }
}

impl Drop for CostFixture {
    fn drop(&mut self) {
        for actor in &self.actors {
            actor.abort();
        }
    }
}
