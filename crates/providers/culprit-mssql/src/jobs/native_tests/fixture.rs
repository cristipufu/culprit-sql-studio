//! Fixture-only Agent administration is bounded and admitted independently from the read-only source.
use super::*;
use crate::conn::{MsClient, map_error};
const CONTROL_TIMEOUT: Duration = Duration::from_secs(3);
const POLL_INTERVAL: Duration = Duration::from_millis(250);
const MAX_POLLS: usize = 120;
const JOB_WAIT_TIMEOUT: Duration = Duration::from_secs(30);
const CLEANUP_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_CONTROL_ROWS: usize = 4;
const MAX_CONTROL_BYTES: usize = 8 * 1024;

pub(super) struct AgentJob<'a> {
    fixture: &'a OwnedSourceFixture,
    name: String,
    id: Option<String>,
    creation_requested: bool,
    client: Option<MsClient>,
    queries: u32,
}
impl<'a> AgentJob<'a> {
    pub fn new(fixture: &'a OwnedSourceFixture) -> Result<Self> {
        let identity = culprit_read::admission::observation_identity()?;
        let owner = std::env::var("CULPRIT_SMOKE_OWNER").map_err(|_| invalid_data())?;
        // OwnedSourceFixture has already checked the full nonce against the native machine/database.
        let name = format!("culprit_agent_{owner}_{}", &digest(identity.observation_id.as_bytes())[..16]);
        Ok(Self { fixture, name, id: None, creation_requested: false, client: None, queries: 0 })
    }
    pub fn id(&self) -> Option<&str> {
        self.id.as_deref()
    }
    pub fn queries(&self) -> u32 {
        self.queries
    }
    pub async fn require_agent(&mut self, cancellation: &CollectionCancellation) -> Result<()> {
        let deadline = Instant::now() + JOB_WAIT_TIMEOUT;
        for _ in 0..MAX_POLLS {
            if cancellation.is_cancelled() {
                return Err(CollectionStop::Cancelled.error());
            }
            let rows=self.query(tiberius::Query::new("SELECT TOP (2) /* culprit fixture Agent precondition */ servicename,status FROM sys.dm_server_services WHERE servicename LIKE N'SQL Server Agent (%' AND status=4 ORDER BY servicename")).await?;
            if rows.len() == 1 && crate::read::integer(&rows[0], "status")? == 4 {
                return Ok(());
            }
            if Instant::now() >= deadline {
                break;
            }
            tokio::select! {biased;_ = cancellation.cancelled()=>return Err(CollectionStop::Cancelled.error()),_ = tokio::time::sleep(POLL_INTERVAL)=>{}}
        }
        Err(unsupported())
    }
    pub async fn create(&mut self) -> Result<()> {
        let mut absent = tiberius::Query::new(
            "SELECT TOP (1) /* culprit fixture ownership */ job_id FROM msdb.dbo.sysjobs WHERE name=@P1",
        );
        absent.bind(self.name.clone());
        if !self.query(absent).await?.is_empty() {
            return Err(CollectionStop::SourceChanged.error());
        }
        self.creation_requested = true;
        let mut query = tiberius::Query::new(
            r#"/* culprit fixture setup only */ DECLARE @job uniqueidentifier;
EXEC msdb.dbo.sp_add_job @job_name=@P1,@enabled=1,@job_id=@job OUTPUT;
EXEC msdb.dbo.sp_add_jobstep @job_id=@job,@step_id=1,@step_name=N'owned failure',@subsystem=N'TSQL',@database_name=@P2,@command=N'THROW 51000, N''owned Agent fixture failure'', 1;',@on_success_action=1,@on_fail_action=2;
EXEC msdb.dbo.sp_add_jobserver @job_id=@job;
SELECT /* culprit fixture */ CONVERT(varchar(36),@job) AS job_id;"#,
        );
        query.bind(self.name.clone());
        query.bind(self.fixture.database.clone());
        let rows = self.query(query).await?;
        let [row] = rows.as_slice() else {
            return Err(invalid_data());
        };
        self.id = Some(crate::read::text(row, "job_id")?);
        self.confirm_identity().await
    }
    async fn confirm_identity(&mut self) -> Result<()> {
        let mut query = tiberius::Query::new(
            "SELECT TOP (2) /* culprit fixture */ CONVERT(varchar(36),job_id) AS job_id,name FROM msdb.dbo.sysjobs WHERE job_id=CONVERT(uniqueidentifier,@P1) AND name=@P2",
        );
        query.bind(self.id.clone().ok_or_else(invalid_data)?);
        query.bind(self.name.clone());
        let rows = self.query(query).await?;
        let [row] = rows.as_slice() else {
            return Err(invalid_data());
        };
        if crate::read::text(row, "job_id")? != self.id.as_deref().ok_or_else(invalid_data)?
            || crate::read::text(row, "name")? != self.name
        {
            return Err(invalid_data());
        }
        Ok(())
    }
    pub async fn make_healthy(&mut self) -> Result<()> {
        self.confirm_identity().await?;
        let mut query = tiberius::Query::new(
            "/* culprit fixture update only */ EXEC msdb.dbo.sp_update_jobstep @job_id=@P1,@step_id=1,@step_name=N'owned healthy',@command=N'SELECT 1 AS owned_agent_fixture_value;';SELECT /* culprit fixture */ 1 AS updated;",
        );
        query.bind(self.id.clone().ok_or_else(invalid_data)?);
        self.query(query).await?;
        Ok(())
    }
    pub async fn run_and_wait(&mut self, after: i64, cancellation: &CollectionCancellation) -> Result<(i64, i64)> {
        if cancellation.is_cancelled() {
            return Err(CollectionStop::Cancelled.error());
        }
        self.confirm_identity().await?;
        let mut query = tiberius::Query::new(
            "/* culprit fixture work only */ EXEC msdb.dbo.sp_start_job @job_id=@P1;SELECT /* culprit fixture */ 1 AS started;",
        );
        query.bind(self.id.clone().ok_or_else(invalid_data)?);
        self.query(query).await?;
        let deadline = Instant::now() + JOB_WAIT_TIMEOUT;
        for _ in 0..MAX_POLLS {
            if Instant::now() >= deadline {
                self.report_pending_activity().await?;
                return Err(CollectionStop::TimedOut.error());
            }
            if cancellation.is_cancelled() {
                return Err(CollectionStop::Cancelled.error());
            }
            let mut query = tiberius::Query::new(
                "SELECT TOP (1) /* culprit fixture */ instance_id,run_status FROM msdb.dbo.sysjobhistory WHERE job_id=CONVERT(uniqueidentifier,@P1) AND step_id=0 AND instance_id>@P2 ORDER BY instance_id DESC",
            );
            query.bind(self.id.clone().ok_or_else(invalid_data)?);
            query.bind(after);
            let rows = self.query(query).await?;
            if let Some(row) = rows.first() {
                return Ok((
                    crate::read::integer(row, "instance_id")?,
                    crate::read::signed_integer(row, "run_status")?,
                ));
            }
            tokio::select! {biased;_ = cancellation.cancelled()=>return Err(CollectionStop::Cancelled.error()),_ = tokio::time::sleep(POLL_INTERVAL)=>{}}
        }
        self.report_pending_activity().await?;
        Err(CollectionStop::TimedOut.error())
    }
    async fn report_pending_activity(&mut self) -> Result<()> {
        let mut query = tiberius::Query::new(
            r#"SELECT TOP (1) /* culprit fixture timeout context */ session_id,
 CONVERT(varchar(40),run_requested_date,126) AS requested,
 CONVERT(varchar(40),queued_date,126) AS queued,
 CONVERT(varchar(40),start_execution_date,126) AS started,
 CONVERT(varchar(40),stop_execution_date,126) AS stopped,
 last_executed_step_id,job_history_id
 FROM msdb.dbo.sysjobactivity WHERE job_id=CONVERT(uniqueidentifier,@P1) ORDER BY session_id DESC"#,
        );
        query.bind(self.id.clone().ok_or_else(invalid_data)?);
        let rows = self.query(query).await?;
        let states = rows
            .iter()
            .map(|row| {
                Ok(json!({
                    "agent_startup_session":crate::read::optional_integer(row,"session_id")?,
                    "requested":crate::read::optional_text(row,"requested")?,
                    "queued":crate::read::optional_text(row,"queued")?,
                    "started":crate::read::optional_text(row,"started")?,
                    "stopped":crate::read::optional_text(row,"stopped")?,
                    "last_executed_step":crate::read::optional_integer(row,"last_executed_step_id")?,
                    "history_id":crate::read::optional_integer(row,"job_history_id")?,
                }))
            })
            .collect::<Result<Vec<_>>>()?;
        eprintln!("owned Agent pending activity: {}", json!(states));
        Ok(())
    }
    pub async fn cleanup(&mut self) -> Result<()> {
        if !self.creation_requested {
            self.client.take();
            return Ok(());
        }
        let deadline = Instant::now() + CLEANUP_TIMEOUT;
        // Retire any prior admin connection. Cleanup uses a freshly admitted exact-owned control client.
        self.client.take();
        let mut lookup = tiberius::Query::new(
            "SELECT TOP (2) /* culprit fixture cleanup */ CONVERT(varchar(36),job_id) AS job_id,name FROM msdb.dbo.sysjobs WHERE name=@P1",
        );
        lookup.bind(self.name.clone());
        let rows = self.query(lookup).await?;
        if rows.is_empty() {
            return Ok(());
        }
        let [row] = rows.as_slice() else {
            return Err(invalid_data());
        };
        let actual = crate::read::text(row, "job_id")?;
        if self.id.as_ref().is_some_and(|expected| expected != &actual) || crate::read::text(row, "name")? != self.name
        {
            return Err(invalid_data());
        }
        self.id = Some(actual);
        if self.active().await? {
            let mut stop = tiberius::Query::new(
                "/* culprit fixture cleanup only */ IF EXISTS(SELECT 1 FROM msdb.dbo.sysjobactivity WHERE job_id=CONVERT(uniqueidentifier,@P1) AND session_id=(SELECT MAX(session_id) FROM msdb.dbo.syssessions) AND start_execution_date IS NOT NULL AND stop_execution_date IS NULL) EXEC msdb.dbo.sp_stop_job @job_id=@P1;SELECT /* culprit fixture */ 1 AS requested;",
            );
            stop.bind(self.id.clone().ok_or_else(invalid_data)?);
            self.query(stop).await?;
        }
        for _ in 0..MAX_POLLS {
            if Instant::now() >= deadline {
                break;
            }
            if !self.active().await? {
                return self.delete().await;
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
        // No claimed cleanup or deletion while an owned execution has unresolved shutdown.
        Err(CollectionStop::CancellationUnconfirmed.error())
    }
    async fn active(&mut self) -> Result<bool> {
        let mut query = tiberius::Query::new(
            "SELECT TOP (1) /* culprit fixture */ session_id FROM msdb.dbo.sysjobactivity WHERE job_id=CONVERT(uniqueidentifier,@P1) AND start_execution_date IS NOT NULL AND stop_execution_date IS NULL ORDER BY session_id DESC",
        );
        query.bind(self.id.clone().ok_or_else(invalid_data)?);
        Ok(!self.query(query).await?.is_empty())
    }
    async fn delete(&mut self) -> Result<()> {
        self.confirm_identity().await?;
        let mut query = tiberius::Query::new(
            "/* culprit fixture cleanup only */ EXEC msdb.dbo.sp_delete_job @job_id=@P1,@delete_history=1,@delete_unused_schedule=0;SELECT TOP (2) /* culprit fixture */ job_id FROM msdb.dbo.sysjobs WHERE job_id=CONVERT(uniqueidentifier,@P1) OR name=@P2;",
        );
        query.bind(self.id.clone().ok_or_else(invalid_data)?);
        query.bind(self.name.clone());
        let rows = self.query(query).await?;
        if !rows.is_empty() {
            return Err(invalid_data());
        }
        if let Some(client) = self.client.take() {
            client.close().await.map_err(map_error)?;
        }
        Ok(())
    }
    async fn query(&mut self, query: tiberius::Query<'_>) -> Result<Vec<Rec>> {
        if self.client.is_none() {
            let client = tokio::time::timeout(CONTROL_TIMEOUT, async {
                let mut client = connect(
                    &config(self.fixture.port, "msdb", self.fixture.password(), "owned-agent-fixture-control"),
                    SessionKind::User,
                )
                .await?;
                verify_owned_source(&mut client).await?;
                Ok::<_, Error>(client)
            })
            .await
            .map_err(|_| CollectionStop::CancellationUnconfirmed.error())??;
            self.client = Some(client);
        }
        let mut client = self.client.take().ok_or_else(invalid_data)?;
        self.queries += 1;
        let result = tokio::time::timeout(CONTROL_TIMEOUT, async {
            let results = query.query(&mut client).await.map_err(map_error)?.into_results().await.map_err(map_error)?;
            let rows = results.into_iter().flatten().map(|row| Rec::from_row(&row)).collect::<Vec<_>>();
            if rows.len() > MAX_CONTROL_ROWS
                || rows.iter().map(crate::read::value_bytes).sum::<usize>() > MAX_CONTROL_BYTES
            {
                return Err(invalid_data());
            }
            Ok::<_, Error>(rows)
        })
        .await;
        match result {
            Ok(Ok(rows)) => {
                self.client = Some(client);
                Ok(rows)
            }
            Ok(Err(error)) => Err(error),
            Err(_) => Err(CollectionStop::CancellationUnconfirmed.error()),
        }
        // On every failed/undrained control read the client drops here and is never reused.
    }
}
