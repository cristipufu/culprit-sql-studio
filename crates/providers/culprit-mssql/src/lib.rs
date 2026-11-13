//! SQL Server provider: reads Query Store, the plan cache and catalog/DMV statistics into a Culprit snapshot.
//! Every statistics query is read-only and runs with a short lock timeout and low deadlock priority.

mod activity;
mod actual;
mod ado;
mod advice;
mod cache_summary;
mod cached_definitions;
mod cached_payload;
mod cached_runtime;
mod cached_statements;
mod catalog_expression;
mod catalog_visibility;
mod check_constraints;
mod collect;
mod columns;
mod compiler_gateways;
mod conn;
mod constraints;
pub mod deadlocks;
mod foreign_keys;
mod grants;
mod index_operations;
mod indexes;
mod live;
mod missing_indexes;
mod open_transactions;
mod operations;
mod partition_storage;
mod partitions;
mod performance;
mod periods;
pub mod plan_xml;
mod pressure;
mod probe;
mod processors;
mod query_store;
mod queues;
mod read;
mod read_metrics;
mod recorded;
mod replication;
mod resources;
mod retained_definitions;
mod settings;
mod signals;
pub mod sources;
mod spread;
mod sql_time;
mod statement;
mod statement_origin;
mod statistics;
mod storage_io;
mod tables;
mod telemetry;
#[cfg(test)]
mod test_support;
mod timeline;
mod triggers;
mod value;
mod version_store;
mod waits;
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
    pool: std::sync::Arc<Pool>,
}

impl MssqlProvider {
    pub fn new(spec: &ConnectionSpec) -> Result<Self> {
        Ok(MssqlProvider {
            environment: spec.environment,
            pool: std::sync::Arc::new(Pool::new(conn::build_config(spec)?, spec.transport.clone())),
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
    fn telemetry_source(
        &self,
        request: &culprit_core::telemetry::CollectionRequest,
    ) -> Result<Box<dyn culprit_core::telemetry::TelemetrySource>> {
        telemetry::source(self.pool.clone(), request, self.endpoint_host.clone(), self.endpoint_port)
    }
    async fn telemetry_intervals(
        &self,
        request: &culprit_core::telemetry::IntervalDiscoveryRequest,
    ) -> Result<culprit_core::telemetry::IntervalDiscoveryPage> {
        telemetry::discover(self.pool.clone(), &self.endpoint_host, self.endpoint_port, request).await
    }
    fn source_shutdown(&self) -> culprit_core::telemetry::SourceShutdown {
        self.pool.source_shutdown()
    }

    fn collector_version(&self) -> culprit_core::ComponentVersion {
        component_version()
    }

    fn engine(&self) -> Engine {
        Engine::Mssql
    }

    async fn probe(&self) -> Result<ServerInfo> {
        let probe = self.pool.observe(|c| Box::pin(probe::run(c))).await?;
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
        live::collect(self.pool.clone(), self.endpoint_host.clone(), self.endpoint_port).await
    }

    async fn lock_waits(&self) -> Result<Vec<culprit_core::LockWait>> {
        self.pool.observe(|c| Box::pin(deadlocks::lock_waits(c))).await
    }

    async fn deadlocks(&self, since_hours: f64) -> Result<culprit_core::DeadlockReport> {
        let budget = culprit_core::telemetry::CollectionBudget { page_bytes: 8 * 1024 * 1024, ..Default::default() };
        let mut reader = read::ReadSession::new(self.pool.clone(), budget);
        let result = deadlocks::recent(&mut reader, since_hours).await;
        if reader.close().await == culprit_core::telemetry::SourceShutdown::Unconfirmed {
            return Err(culprit_core::telemetry::CollectionStop::CancellationUnconfirmed.error());
        }
        result
    }

    async fn recorded_changes(&self, since: &str) -> Result<Vec<culprit_core::ChangeEvent>> {
        let since = since.to_string();
        self.pool
            .observe(move |c| {
                let since = since.clone();
                Box::pin(async move { recorded::changes(c, &since).await })
            })
            .await
    }

    async fn wait_sample(&self, seconds: u32) -> Result<culprit_core::WaitSample> {
        let probe = self.pool.observe(|c| Box::pin(probe::run(c))).await?;
        if !probe.can_read_dmvs() {
            return Err(waits_unreadable(probe.grant_hint()));
        }
        let azure = probe.is_azure_sql_database();
        let budget = culprit_core::telemetry::CollectionBudget {
            total_timeout_ms: seconds.saturating_mul(1000).saturating_add(60_000).min(900_000),
            ..Default::default()
        };
        let mut reader = read::ReadSession::new(self.pool.clone(), budget);
        let result = pressure::sample(&mut reader, azure, seconds).await;
        if reader.close().await == culprit_core::telemetry::SourceShutdown::Unconfirmed {
            return Err(culprit_core::telemetry::CollectionStop::CancellationUnconfirmed.error());
        }
        result
    }

    async fn interrupt_session(&self, session: &str, action: SessionAction) -> Result<()> {
        live::interrupt(&self.pool, session, action).await
    }

    async fn period_stats(
        &self,
        baseline: &culprit_core::changes::Period,
        recent: &culprit_core::changes::Period,
    ) -> Result<Option<culprit_core::changes::PeriodData>> {
        let mut reader = query_store::Reader::open(self.pool.clone());
        let result = periods::collect(&mut reader, baseline, recent).await;
        if reader.close().await == culprit_core::telemetry::SourceShutdown::Unconfirmed {
            return Err(culprit_core::telemetry::CollectionStop::CancellationUnconfirmed.error());
        }
        result
    }

    async fn stored_plan(&self, query_id: &str, plan_id: &str) -> Result<Option<Plan>> {
        let numeric = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit());
        if !numeric(query_id) || !numeric(plan_id) {
            return Err(actual::ids_are_numbers());
        }
        let mut reader = query_store::Reader::open(self.pool.clone());
        let result =
            reader.definitions(&[plan_id.to_owned()], query_store::definitions::Kind::Plan, 2 * 1024 * 1024).await;
        if reader.close().await == culprit_core::telemetry::SourceShutdown::Unconfirmed {
            return Err(culprit_core::telemetry::CollectionStop::CancellationUnconfirmed.error());
        }
        let definitions = result?;
        let Some(definition) = definitions
            .into_iter()
            .find(|definition| definition.plan.as_ref().is_some_and(|plan| plan.query.query_id == query_id))
        else {
            return Ok(None);
        };
        definition.payload.text().map(plan_xml::parse).transpose()
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
        culprit_core::t!("provider.mssql.waits.unreadable"),
    )
    .with_hint(grant)
}

/// The task running a table preview ended without an answer.
pub(crate) fn preview_stopped() -> culprit_core::Error {
    culprit_core::Error::internal(culprit_core::t!("provider.mssql.preview_stopped"))
}
