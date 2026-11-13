use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::actual::QueryProgress;
use crate::error::{Error, ErrorCode, Result};
use crate::model::{Engine, LiveState, ResultSet, ServerInfo, SessionAction, Snapshot};
use crate::plan::Plan;

#[derive(Serialize, Deserialize, TS, Clone, Debug)]
#[ts(export)]
pub struct SnapshotOptions {
    /// How far back to read workload history where the engine keeps it (Query Store), in minutes: `--window 6h` is
    /// 360, the default week 10080.
    pub window_minutes: u32,
    /// How many of the heaviest queries to keep in the snapshot.
    pub top_queries: u32,
    /// How many of the heaviest queries to fetch and parse plans for.
    pub plans: u32,
    /// Explicit opt-in to detailed shared-resource readers that require separate production load qualification.
    #[serde(default)]
    #[ts(optional = nullable)]
    pub collect_shared_resources: bool,
    /// Explicit opt-in to current request-memory participants. Native source cost requires separate qualification.
    #[serde(default)]
    #[ts(optional = nullable)]
    pub collect_request_memory: bool,
}

impl Default for SnapshotOptions {
    fn default() -> Self {
        SnapshotOptions {
            window_minutes: 7 * crate::MINUTES_PER_DAY,
            top_queries: 60,
            plans: 25,
            collect_shared_resources: false,
            collect_request_memory: false,
        }
    }
}

impl SnapshotOptions {
    /// The history window in minutes, a minute at least.
    pub fn window_in_minutes(&self) -> u32 {
        self.window_minutes.max(1)
    }

    /// These options with a window of `minutes` (a minute at least).
    pub fn with_window_minutes(self, minutes: u32) -> Self {
        SnapshotOptions { window_minutes: minutes.max(1), ..self }
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
#[ts(export)]
pub enum ExplainMode {
    /// Plan only. Never executes the statement.
    Estimated,
    /// Executes the statement and reports what actually happened.
    Analyze,
}

/// One database engine. Implementations open their own connections from the configuration they were built with.
/// Interactive futures must release their sessions when dropped. Native work must observe an
/// [`crate::limits::OperationGuard`] handle; result collectors use [`crate::limits::ResultBudget`] and drain
/// completion/errors even after display retention stops.
#[async_trait]
pub trait Provider: Send + Sync {
    fn engine(&self) -> Engine;

    /// Native work across this provider's sessions must be settled before another observation starts.
    /// Implementations that retain workers override this state; an uncertain cancellation must never be hidden in
    /// a successful snapshot or collector note. A closed socket alone does not prove native shutdown.
    fn source_shutdown(&self) -> crate::telemetry::SourceShutdown {
        crate::telemetry::SourceShutdown::Quiescent
    }

    /// Open an independent, bounded telemetry observation. This opt-in source is separate from report scans.
    /// Construction must not start database work. Async source methods own their native work until `close` settles it.
    /// The request selects a logical collection, independently of this provider's engine and any export destination.
    /// Unsupported collections must return an explicit error; they must not manufacture successful empty streams.
    fn telemetry_source(
        &self,
        _request: &crate::telemetry::CollectionRequest,
    ) -> Result<Box<dyn crate::telemetry::TelemetrySource>> {
        Err(Error::new(ErrorCode::Unsupported, crate::t!("collection.unsupported")))
    }

    /// Discover bounded native closed intervals for scheduling. Keep native boundaries, identity and reset evidence;
    /// an unavailable history source is an error. Owned native work must settle before returning a successful page.
    async fn telemetry_intervals(
        &self,
        _request: &crate::telemetry::IntervalDiscoveryRequest,
    ) -> Result<crate::telemetry::IntervalDiscoveryPage> {
        Err(Error::new(ErrorCode::Unsupported, crate::t!("collection.unsupported")))
    }

    /// Resolve native shared-resource identities and supported collection routes using bounded metadata reads.
    /// Permission failures and unsupported kinds are explicit; connection aliases never establish native identity.
    async fn telemetry_resources(
        &self,
        _request: &crate::telemetry::ResourceDiscoveryRequest,
    ) -> Result<Vec<crate::telemetry::CollectionResource>> {
        Err(Error::new(ErrorCode::Unsupported, crate::t!("collection.unsupported")))
    }

    /// The collector that actually supplied this snapshot. Runtime wrappers need not claim a builtin's version.
    fn collector_version(&self) -> crate::ComponentVersion {
        crate::ComponentVersion::unknown("external provider")
    }

    /// Connect, identify the server and report which capabilities this login can use. Cheap; used by `conn test` and `doctor`.
    async fn probe(&self) -> Result<ServerInfo>;

    /// Collect a normalized snapshot. Reads statistics views only; must be safe on production. Bind the database's
    /// identity on it (`Snapshot::bind_identity`, with `DatabaseLocation::Unknown` when the location can't be told): a
    /// scan without identity compares with no other. Its query plans carry their facts, as [`Provider::explain`]'s do.
    async fn snapshot(&self, connection: &str, options: &SnapshotOptions) -> Result<Snapshot>;

    /// The plan of `sql`. A plan carries its facts (`Plan::facts`, each step's `PlanNode::facts`): the inputs every
    /// check and insight reads. A provider for an engine Culprit parses hands the engine's native plan text to
    /// `culprit_engine::plans::parse_plan`; one for another engine fills the facts in, leaving unknown what it doesn't
    /// know. The plan's display properties are for reading and stand in for no fact.
    async fn explain(&self, sql: &str, mode: ExplainMode) -> Result<Plan>;

    /// The estimated plan of a statement someone else's session recorded (a query from a scan, a session's current
    /// statement), never run. Unlike [`Provider::explain`], whoever asks didn't write its text, so an engine whose
    /// planner runs the code a statement reaches may plan the login's own copy instead (`query_id`, its id in the
    /// engine's statistics) or refuse. Everyone else plans it as `explain` would.
    async fn explain_recorded(&self, sql: &str, _query_id: Option<&str>) -> Result<Plan> {
        self.explain(sql, ExplainMode::Estimated).await
    }

    /// Estimated plan of `sql` with one or more virtual indexes (`CREATE INDEX ...`) registered only in this session's
    /// planner memory (PostgreSQL/YSQL `hypopg`). Never creates a real index or modifies the database.
    async fn explain_hypothetical(&self, _sql: &str, _indexes: &[String]) -> Result<Plan> {
        Err(Error::new(
            ErrorCode::Unsupported,
            format!("{} does not support virtual index planning (--with-index)", self.engine().label()),
        )
        .with_hint("Virtual index planning is supported on PostgreSQL when the `hypopg` extension is installed."))
    }

    async fn live(&self) -> Result<LiveState>;

    /// Run a statement that reads and return up to `limit` rows, inside a read-only transaction where the engine has one.
    /// Callers enforce environment rules before calling.
    async fn run(&self, sql: &str, limit: usize) -> Result<ResultSet>;

    /// A table preview: the same read-only guarantees, with a short server-side execution budget. Built-in drivers
    /// apply PREVIEW_TIMEOUT and cancel native work. External providers opt in by implementing a bounded path.
    async fn preview(&self, _sql: &str, _limit: usize) -> Result<ResultSet> {
        Err(Error::new(ErrorCode::Unsupported, "Quick table previews aren't available with this provider")
            .with_hint("Open the query in the SQL editor for this connection"))
    }

    /// Run a statement that changes data (or that Culprit can't prove only reads) in a read-write session and commit it.
    /// Callers have already asked for confirmation where the environment requires it.
    async fn execute(&self, _sql: &str, _limit: usize) -> Result<ResultSet> {
        Err(self.engine().refused(crate::model::Ability::Writes))
    }

    /// The statements the server itself would run for `sql`, by type (`SELECT`, `COMMIT TRANSACTION`, `CREATE TABLE`…),
    /// read without running anything; `None` when the engine (or this login) can't tell. Confirms Culprit's own
    /// classifier with the server's parser before a read or a measured run, so a lexing difference can't hide a
    /// second statement.
    async fn statement_types(&self, _sql: &str) -> Result<Option<Vec<String>>> {
        Ok(None)
    }

    /// The lock each waiting session asked for (object, index, mode), to explain blocking chains. Read only when someone
    /// is blocked. Empty when the engine can't tell.
    async fn lock_waits(&self) -> Result<Vec<crate::blocking::LockWait>> {
        Ok(Vec::new())
    }

    /// Recent deadlocks from wherever the engine keeps them, newest first, already classified.
    async fn deadlocks(&self, _since_hours: f64) -> Result<crate::deadlock::DeadlockReport> {
        Err(self.engine().refused(crate::model::Ability::Deadlocks))
    }

    /// What the server waits on right now: two readings `seconds` apart (or sessions sampled every second). Reads
    /// statistics views only; safe on production.
    async fn wait_sample(&self, _seconds: u32) -> Result<crate::waits::WaitSample> {
        Err(self.engine().refused(crate::model::Ability::WaitSample))
    }

    /// Every query's runs and time in two periods, from a source that keeps history by interval (Query Store).
    /// `None` when the engine only keeps running totals; callers then compare Culprit's own scans.
    async fn period_stats(
        &self,
        _baseline: &crate::changes::Period,
        _recent: &crate::changes::Period,
    ) -> Result<Option<crate::changes::PeriodData>> {
        Ok(None)
    }

    /// A plan the engine kept for a query (Query Store keeps every plan a query used). `None` when the engine keeps no plans.
    async fn stored_plan(&self, _query_id: &str, _plan_id: &str) -> Result<Option<Plan>> {
        Ok(None)
    }

    /// The actual plan of a query's most recent run as the engine kept it, without running the query (SQL Server 2019+
    /// with LAST_QUERY_PLAN_STATS). Ids are the workload's query and plan ids.
    async fn last_actual_plan(&self, _query_id: &str, _plan_id: Option<&str>) -> Result<Plan> {
        Err(self.engine().refused(crate::model::Ability::LastActualPlan))
    }

    /// How far along another session's running statement is. `with_plan` asks for the plan as well; engines whose
    /// plan doesn't change while it runs (Postgres, MySQL) only send it when asked, SQL Server always sends its live
    /// counters.
    async fn query_progress(&self, _session: &str, _with_plan: bool) -> Result<QueryProgress> {
        Err(Error::new(
            ErrorCode::Unsupported,
            format!("{} can't report a running statement's progress", self.engine()),
        ))
    }

    /// What `interrupt_session` supports on this engine: the engine's own (`EngineSpec::session_actions`) unless this
    /// provider can do less.
    fn session_actions(&self) -> Vec<SessionAction> {
        self.engine().spec().session_actions()
    }

    /// Cancel another session's statement or end the session. `session` is an id from `live()`. Implementations refuse
    /// Culprit's own sessions and system sessions; callers enforce environment rules (never on production).
    async fn interrupt_session(&self, _session: &str, _action: SessionAction) -> Result<()> {
        Err(Error::new(ErrorCode::Unsupported, format!("{} sessions can't be ended from Culprit", self.engine())))
    }

    /// Changes the database timestamps itself since `since` (UTC, ISO 8601): tables, procedures and views created or
    /// altered, settings changed at runtime, the configuration reloaded. Catalog views only; safe on production. Changes
    /// the engine doesn't date are found by comparing Culprit's scans instead.
    async fn recorded_changes(&self, _since: &str) -> Result<Vec<crate::events::ChangeEvent>> {
        Err(Error::new(ErrorCode::Unsupported, format!("{} doesn't date schema changes", self.engine())))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_window_is_kept_in_minutes_a_minute_at_least() {
        let six_hours = SnapshotOptions::default().with_window_minutes(360);
        assert_eq!(six_hours.window_minutes, 360);
        assert_eq!(SnapshotOptions::default().window_in_minutes(), 7 * crate::MINUTES_PER_DAY);
        assert_eq!(SnapshotOptions::default().with_window_minutes(0).window_in_minutes(), 1);
        assert_eq!(SnapshotOptions { window_minutes: 0, ..Default::default() }.window_in_minutes(), 1);
    }
    #[test]
    fn shared_resource_collection_requires_an_explicit_request() {
        assert!(!SnapshotOptions::default().collect_shared_resources);
        let request = serde_json::json!({"window_minutes":60,"top_queries":20,"plans":0});
        let options: SnapshotOptions = serde_json::from_value(request.clone()).unwrap();
        assert!(!options.collect_shared_resources);
        let mut request = request;
        request["collect_shared_resources"] = true.into();
        let options: SnapshotOptions = serde_json::from_value(request).unwrap();
        assert!(options.with_window_minutes(90).collect_shared_resources);
        assert!(SnapshotOptions::decl(&ts_rs::Config::default()).contains("collect_shared_resources?: boolean"));
    }
    #[test]
    fn request_memory_collection_requires_its_own_explicit_request() {
        assert!(!SnapshotOptions::default().collect_request_memory);
        let mut request = serde_json::json!({"window_minutes":60,"top_queries":20,"plans":0});
        let options: SnapshotOptions = serde_json::from_value(request.clone()).unwrap();
        assert!(!options.collect_request_memory);
        request["collect_request_memory"] = true.into();
        let options: SnapshotOptions = serde_json::from_value(request).unwrap();
        assert!(!options.collect_shared_resources);
        assert!(options.with_window_minutes(90).collect_request_memory);
        assert!(SnapshotOptions::decl(&ts_rs::Config::default()).contains("collect_request_memory?: boolean"));
    }
}
