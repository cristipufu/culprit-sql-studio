//! SQL Server provider: reads Query Store, the plan cache and catalog/DMV statistics into a Culprit snapshot.
//! Every statistics query is read-only and runs with a short lock timeout and low deadlock priority.

mod actual;
mod ado;
mod advice;
mod collect;
mod conn;
pub mod deadlocks;
mod live;
mod operations;
mod periods;
pub mod plan_xml;
mod pressure;
mod probe;
mod recorded;
mod schema;
mod signals;
pub mod sources;
mod spread;
mod statement;
mod timeline;
mod value;
#[cfg(test)]
mod words;
mod workload;

use async_trait::async_trait;
use culprit_core::{
    ConnectionSpec, Engine, Environment, ExplainMode, LiveState, Plan, Provider, Result, ResultSet, ServerInfo,
    SessionAction, Snapshot, SnapshotOptions,
};

pub use ado::parse_ado;
use conn::{MsClient, Pool, SessionKind};

/// Identity of this crate's collector and native-plan parser, including a build revision when supplied.
pub fn component_version() -> culprit_core::ComponentVersion {
    culprit_core::ComponentVersion::package(
        "culprit-mssql",
        env!("CARGO_PKG_VERSION"),
        option_env!("CULPRIT_BUILD_REVISION").or(option_env!("GITHUB_SHA")),
    )
}

pub struct MssqlProvider {
    endpoint_host: String,
    endpoint_port: u16,
    environment: Environment,
    pool: Pool,
}

impl MssqlProvider {
    pub fn new(spec: &ConnectionSpec) -> Result<Self> {
        Ok(MssqlProvider {
            environment: spec.environment,
            pool: Pool::new(conn::build_config(spec)?, spec.transport.clone()),
            endpoint_host: spec.host.trim().into(),
            endpoint_port: spec.port.unwrap_or(conn::DEFAULT_PORT),
        })
    }

    async fn user_session(&self) -> Result<MsClient> {
        self.pool.connect(SessionKind::User).await
    }
}

#[async_trait]
impl Provider for MssqlProvider {
    fn collector_version(&self) -> culprit_core::ComponentVersion {
        component_version()
    }

    fn engine(&self) -> Engine {
        Engine::Mssql
    }

    async fn probe(&self) -> Result<ServerInfo> {
        let probe = self.pool.with(|c| Box::pin(probe::run(c))).await?;
        let (have, missing) = probe::expected_capabilities(&probe);
        Ok(probe::server_info(&probe, have, missing))
    }

    async fn snapshot(&self, connection: &str, options: &SnapshotOptions) -> Result<Snapshot> {
        collect::snapshot(self, connection, options).await
    }

    async fn explain(&self, sql: &str, mode: ExplainMode) -> Result<Plan> {
        let mut client = self.user_session().await?;
        match mode {
            ExplainMode::Estimated => statement::estimated_plan(&mut client, sql).await,
            ExplainMode::Analyze => statement::actual_plan(&mut client, sql).await,
        }
        .map_err(statement::parameter_error)
    }

    async fn live(&self) -> Result<LiveState> {
        self.pool.with(|c| Box::pin(live::collect(c))).await
    }

    async fn lock_waits(&self) -> Result<Vec<culprit_core::LockWait>> {
        self.pool.with(|c| Box::pin(deadlocks::lock_waits(c))).await
    }

    async fn deadlocks(&self, since_hours: f64) -> Result<culprit_core::DeadlockReport> {
        self.pool.with(move |c| Box::pin(deadlocks::recent(c, since_hours))).await
    }

    async fn recorded_changes(&self, since: &str) -> Result<Vec<culprit_core::ChangeEvent>> {
        let since = since.to_string();
        self.pool
            .with(move |c| {
                let since = since.clone();
                Box::pin(async move { recorded::changes(c, &since).await })
            })
            .await
    }

    async fn wait_sample(&self, seconds: u32) -> Result<culprit_core::WaitSample> {
        let probe = self.pool.with(|c| Box::pin(probe::run(c))).await?;
        if !probe.can_read_dmvs() {
            return Err(waits_unreadable(probe.grant_hint()));
        }
        let azure = probe.is_azure_sql_database();
        self.pool.with(move |c| Box::pin(pressure::sample(c, azure, seconds))).await
    }

    async fn interrupt_session(&self, session: &str, action: SessionAction) -> Result<()> {
        live::interrupt(&self.pool, session, action).await
    }

    async fn period_stats(
        &self,
        baseline: &culprit_core::changes::Period,
        recent: &culprit_core::changes::Period,
    ) -> Result<Option<culprit_core::changes::PeriodData>> {
        self.pool
            .with(|c| {
                let (baseline, recent) = (baseline.clone(), recent.clone());
                Box::pin(async move { periods::collect(c, &baseline, &recent).await })
            })
            .await
    }

    async fn stored_plan(&self, query_id: &str, plan_id: &str) -> Result<Option<Plan>> {
        let numeric = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit());
        if !numeric(query_id) || !numeric(plan_id) {
            return Err(actual::ids_are_numbers());
        }
        let sql = format!(
            "SELECT query_plan FROM sys.query_store_plan WHERE plan_id = {plan_id} AND query_id = {query_id} /* culprit */"
        );
        let xml = self
            .pool
            .with(|c| {
                let sql = sql.clone();
                Box::pin(async move { conn::query(c, &sql).await })
            })
            .await?
            .into_iter()
            .next()
            .and_then(|r| r.str("query_plan"));
        xml.map(|x| plan_xml::parse(&x)).transpose()
    }

    async fn last_actual_plan(&self, query_id: &str, plan_id: Option<&str>) -> Result<Plan> {
        actual::last_actual_plan(&self.pool, query_id, plan_id).await
    }

    async fn query_progress(&self, session: &str, _with_plan: bool) -> Result<culprit_core::QueryProgress> {
        actual::query_progress(&self.pool, session).await
    }

    async fn run(&self, sql: &str, limit: usize) -> Result<ResultSet> {
        let mut client = self.user_session().await?;
        statement::run(&mut client, sql, limit).await.map_err(statement::parameter_error)
    }

    async fn preview(&self, sql: &str, limit: usize) -> Result<ResultSet> {
        let mut client = self.user_session().await?;
        let sql = sql.to_string();
        let transport = self.pool.transport_owner();
        // The task retains the client so dropping its caller still sends Attention and closes the session.
        let (owner, canceled) = tokio::sync::oneshot::channel::<()>();
        let task = tokio::spawn(async move {
            // Keep an SSH route alive long enough to deliver cancellation after its provider has been dropped.
            let _transport = transport;
            let result = tokio::select! {
                biased;
                _ = canceled => None,
                result = tokio::time::timeout(culprit_core::limits::PREVIEW_TIMEOUT, statement::run(&mut client, &sql, limit)) => Some(result),
            };
            match result {
                Some(Ok(result)) => result.map_err(statement::parameter_error),
                _ => {
                    let _ = tokio::time::timeout(std::time::Duration::from_secs(1), client.cancel_query()).await;
                    Err(culprit_core::limits::timeout_error())
                }
            }
        });
        let result = task.await.map_err(|_| preview_stopped())?;
        drop(owner);
        result
    }

    async fn statement_types(&self, sql: &str) -> Result<Option<Vec<String>>> {
        let mut client = self.user_session().await?;
        match statement::statement_types(&mut client, sql).await {
            Ok(types) => Ok(Some(types)),
            // A login without SHOWPLAN permission can't ask; Culprit's own classifier has already passed the text.
            Err(e) if e.message.to_ascii_lowercase().contains("showplan permission") => Ok(None),
            Err(e) => Err(statement::parameter_error(e)),
        }
    }

    /// SQL Server has no read-only transactions, so reads and writes share one path (autocommit); the Workspace only
    /// sends statements here that the classifier proved read, or that the person confirmed.
    async fn execute(&self, sql: &str, limit: usize) -> Result<ResultSet> {
        let mut client = self.user_session().await?;
        if !statement::returns_rows(sql) {
            // "2 rows changed, committed." — the same words MySQL and PostgreSQL use.
            return statement::run_changes(&mut client, sql).await.map_err(statement::parameter_error);
        }
        let mut result = statement::run(&mut client, sql, limit).await.map_err(statement::parameter_error)?;
        result.messages.push(statement::committed());
        Ok(result)
    }
}

/// Sampling waits without permission to read them; `grant` gives it.
pub(crate) fn waits_unreadable(grant: String) -> culprit_core::Error {
    culprit_core::Error::new(
        culprit_core::ErrorCode::PermissionDenied,
        "Sampling waits needs permission to read the server's wait statistics",
    )
    .with_hint(grant)
}

/// The task running a table preview ended without an answer.
pub(crate) fn preview_stopped() -> culprit_core::Error {
    culprit_core::Error::internal("The table preview worker stopped")
}
