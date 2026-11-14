//! MySQL and MariaDB provider for Culprit: read-only snapshots from performance_schema, information_schema and the
//! slow query log table, plans via EXPLAIN (and MariaDB's ANALYZE), live sessions with lock waits.

mod cancellation;
mod catalog;
mod collect;
mod connect;
pub mod deadlocks;
pub mod fingerprint;
mod json_members;
mod native;
mod native_failures;
mod operations;
pub mod plan_json;
pub mod plan_maria;
pub mod plan_tree;
mod pressure;
mod preview;
mod probe;
mod progress;
mod read;
mod recorded;
mod rows;
mod session_memory;
mod sessions;
mod signals;
mod slowlog;
pub mod sources;
mod telemetry;
mod vars;
mod workload;

use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use culprit_core::{
    ConnectionSpec, Engine, Error, ErrorCode, ExplainMode, LiveState, Plan, PlanKind, Provider, Result, ResultSet,
    ServerInfo, Session, SessionAction, Snapshot, SnapshotOptions, WaitStat, format, t,
};
use mysql_async::consts::ColumnType;
use mysql_async::prelude::Queryable;
use mysql_async::{Conn, Value};
use serde_json::Value as Json;

use connect::{Connector, Flavor, Scope, map_error};
use rows::{explain_one, query, query_one, tagged, value_text};

/// Identity of this crate's collector and native-plan parser, including a build revision when supplied.
pub fn component_version() -> culprit_core::ComponentVersion {
    culprit_core::ComponentVersion::package(
        "culprit-mysql",
        env!("CARGO_PKG_VERSION"),
        option_env!("CULPRIT_BUILD_REVISION").or(option_env!("GITHUB_SHA")),
    )
}

/// Static declarations from the same source registrations used for native MySQL/MariaDB telemetry dispatch.
/// Listing never constructs a provider or establishes native product, permission or production cost availability.
pub fn telemetry_recipes() -> Vec<culprit_core::telemetry::RecipeDescriptor> {
    telemetry::recipes()
}

pub struct MysqlProvider {
    connector: Arc<Connector>,
    spec: ConnectionSpec,
}

impl MysqlProvider {
    pub fn new(spec: &ConnectionSpec) -> Result<Self> {
        if let Some(engine) = spec.engine
            && engine != Engine::Mysql
        {
            return Err(not_mysql(engine));
        }
        Ok(MysqlProvider { connector: Arc::new(Connector::new(spec)?), spec: spec.clone() })
    }

    async fn read(&self, sql: &str, limit: usize) -> Result<ResultSet> {
        let statement = single_statement(sql)?;
        let mut conn = self.connector.connect(Scope::Database).await?;
        let started = Instant::now();
        conn.query_drop(tagged("START TRANSACTION READ ONLY")).await.map_err(map_error)?;
        let result = run_in_transaction(&mut conn, statement, limit, false).await;
        let rollback = conn.query_drop(tagged("ROLLBACK")).await;
        let mut result = result?;
        rollback.map_err(map_error)?;
        let _ = conn.disconnect().await;
        result.elapsed_ms = started.elapsed().as_secs_f64() * 1000.0;
        Ok(result)
    }

    /// The plan, tagged with the server's version so its notes can say what this version can and can't measure.
    fn versioned(&self, mut plan: Plan) -> Plan {
        if let Some(version) = self.connector.version() {
            plan.facts_mut().server_version = Some(version.to_string());
            plan.props.insert("server_version".into(), version.to_string());
        }
        plan
    }
}

impl MysqlProvider {
    async fn explain_statement(&self, sql: &str, mode: ExplainMode) -> Result<Plan> {
        let statement = single_statement(sql)?;
        let schema = self.connector.database.as_deref();
        let mut conn = self.connector.connect(Scope::Database).await?;
        let flavor = self.connector.flavor();
        match mode {
            ExplainMode::Estimated => {
                let verb = leading_verb(statement);
                if !EXPLAINABLE.contains(&verb.as_str()) {
                    return Err(cant_explain(flavor, &verb));
                }
                if flavor == Flavor::Mysql {
                    let _ = conn.query_drop(tagged("SET SESSION explain_json_format_version = 1")).await;
                }
                let row = match explain_one(&mut conn, &format!("EXPLAIN FORMAT=JSON {statement}")).await {
                    // Culprit's sessions are read-only (EXPLAIN can evaluate stored functions), and the server refuses
                    // to plan a write inside one. Nothing would have been written: say what happened instead.
                    Err(e) if WRITES.contains(&verb.as_str()) && e.message.contains("READ ONLY") => {
                        return Err(no_plan_for_write(flavor, &verb, lower_starts_values(statement)));
                    }
                    result => result?.ok_or_else(|| no_plan(PlanCommand::Explain))?,
                };
                let json = row.text("explain").ok_or_else(|| no_plan(PlanCommand::Explain))?;
                plan_json::parse(&json, PlanKind::Estimated, Some(statement), schema).map(|plan| self.versioned(plan))
            }
            ExplainMode::Analyze => {
                if !self.connector.measures_runs() {
                    return Err(cant_measure(self.connector.version().unwrap_or_default()));
                }
                if !reads_only(statement) {
                    return Err(analyze_reads_only());
                }
                // MySQL's EXPLAIN ANALYZE prints a tree; MariaDB's ANALYZE prints the EXPLAIN JSON with measurements.
                let sql = match flavor {
                    Flavor::Mysql => format!("EXPLAIN ANALYZE {statement}"),
                    Flavor::Mariadb => format!("ANALYZE FORMAT=JSON {statement}"),
                };
                conn.query_drop(tagged("START TRANSACTION READ ONLY")).await.map_err(map_error)?;
                let result = explain_one(&mut conn, &sql).await;
                let rollback = conn.query_drop(tagged("ROLLBACK")).await;
                let row = result?.ok_or_else(|| no_plan(PlanCommand::Analyze))?;
                rollback.map_err(map_error)?;
                match flavor {
                    Flavor::Mysql => {
                        let tree = row.text("explain").ok_or_else(|| no_plan(PlanCommand::ExplainAnalyze))?;
                        plan_tree::parse(&tree, Some(statement), schema)
                    }
                    Flavor::Mariadb => {
                        let json = row.first_text().ok_or_else(|| no_plan(PlanCommand::Analyze))?;
                        plan_json::parse(&json, PlanKind::Actual, Some(statement), schema)
                    }
                }
                .map(|mut plan| {
                    plan.facts_mut().execution_protection =
                        Some(culprit_core::plan_facts::ExecutionProtection::ReadOnlyTransaction);
                    plan
                })
            }
        }
    }
}

/// EXPLAIN goes through a prepared statement, where a `?` is a parameter MySQL wants a value for before it plans.
fn parameter_error(e: Error) -> Error {
    if !e.message.contains("parameters but") {
        return e;
    }
    Error::invalid(t!("provider.mysql.explain.parameters")).with_hint(t!("provider.mysql.explain.parameters_hint"))
}

#[async_trait]
impl Provider for MysqlProvider {
    fn collector_version(&self) -> culprit_core::ComponentVersion {
        component_version()
    }

    fn engine(&self) -> Engine {
        Engine::Mysql
    }

    fn source_shutdown(&self) -> culprit_core::telemetry::SourceShutdown {
        self.connector.source_shutdown()
    }

    fn telemetry_source(
        &self,
        request: &culprit_core::telemetry::CollectionRequest,
    ) -> Result<Box<dyn culprit_core::telemetry::TelemetrySource>> {
        telemetry::source(self.connector.clone(), request)
    }

    async fn telemetry_resources(
        &self,
        request: &culprit_core::telemetry::ResourceDiscoveryRequest,
    ) -> Result<Vec<culprit_core::telemetry::CollectionResource>> {
        telemetry::discover(self.connector.clone(), request).await
    }

    async fn probe(&self) -> Result<ServerInfo> {
        let mut conn = crate::read::ReadSession::application(self.connector.clone(), Scope::Server);
        let probe = probe::probe(&mut conn, &self.connector).await?;
        conn.close().await;
        Ok(probe.server_info())
    }

    async fn snapshot(&self, connection: &str, options: &SnapshotOptions) -> Result<Snapshot> {
        collect::snapshot(&self.connector, &self.spec, connection, options).await
    }

    /// A statement the scan recorded is digest text (`COUNT ( * )`, `LIMIT ?`): planned as far as it runs, and when its
    /// placeholders stop it, said so instead of the server's syntax error.
    async fn explain_recorded(&self, sql: &str, _query_id: Option<&str>) -> Result<Plan> {
        let text = fingerprint::runnable(sql);
        self.explain(&text, ExplainMode::Estimated).await.map_err(|e| {
            if !fingerprint::has_placeholders(&text)
                || !matches!(e.code, ErrorCode::QueryFailed | ErrorCode::InvalidInput)
            {
                return e;
            }
            recorded_without_values(self.connector.flavor())
        })
    }

    async fn explain(&self, sql: &str, mode: ExplainMode) -> Result<Plan> {
        let mut plan = self.explain_statement(sql, mode).await.map_err(parameter_error)?;
        if self.connector.flavor() == Flavor::Mariadb {
            plan.facts_mut().flavor = Some("MariaDB".into());
            plan.props.insert("flavor".into(), "MariaDB".into());
        }
        Ok(plan)
    }

    async fn interrupt_session(&self, session: &str, action: SessionAction) -> Result<()> {
        sessions::interrupt(&self.connector, session, action).await
    }

    async fn lock_waits(&self) -> Result<Vec<culprit_core::LockWait>> {
        let mut conn = crate::read::ReadSession::application(self.connector.clone(), Scope::Server);
        query_one(&mut conn, "SELECT VERSION() AS version").await?;
        let out = deadlocks::lock_waits(&mut conn, self.connector.flavor()).await;
        conn.close().await;
        out
    }

    async fn deadlocks(&self, since_hours: f64) -> Result<culprit_core::DeadlockReport> {
        let mut conn = crate::read::ReadSession::application(self.connector.clone(), Scope::Server);
        let out = deadlocks::recent(&mut conn, since_hours).await;
        conn.close().await;
        out
    }

    async fn recorded_changes(&self, since: &str) -> Result<Vec<culprit_core::ChangeEvent>> {
        let Some(database) = self.connector.database.clone() else { return Ok(Vec::new()) };
        // No default database: these catalog reads must not count as the application's statements.
        let mut conn = crate::read::ReadSession::application(self.connector.clone(), Scope::Server);
        let out = recorded::changes(&mut conn, &database, since).await;
        conn.close().await;
        out
    }

    async fn wait_sample(&self, seconds: u32) -> Result<culprit_core::WaitSample> {
        let mut conn = crate::read::ReadSession::application_window(
            self.connector.clone(),
            Scope::Server,
            std::time::Duration::from_secs(seconds.into()),
        );
        let result = crate::pressure::sample(&mut conn, seconds).await;
        conn.close().await;
        result
    }

    /// Servers whose workload comes from the slow query log table compare any two periods straight from the log.
    async fn period_stats(
        &self,
        baseline: &culprit_core::changes::Period,
        recent: &culprit_core::changes::Period,
    ) -> Result<Option<culprit_core::changes::PeriodData>> {
        let mut conn = crate::read::ReadSession::application(self.connector.clone(), Scope::Server);
        let probe = probe::probe(&mut conn, &self.connector).await?;
        if probe.performance_schema || !probe.slow_log_table() {
            conn.close().await;
            return Ok(None);
        }
        let runs = slowlog::read(
            &mut conn,
            &probe.database,
            &slowlog::sql_time(&baseline.start),
            Some(&slowlog::sql_time(&recent.end)),
        )
        .await;
        conn.close().await;
        let runs = runs?;
        let logged_since = runs.iter().map(|r| r.at).reduce(f64::min);
        Ok(Some(slowlog::periods(&runs, baseline, recent, logged_since)))
    }

    async fn query_progress(&self, session: &str, with_plan: bool) -> Result<culprit_core::QueryProgress> {
        progress::query_progress(&self.connector, session, with_plan).await
    }

    async fn live(&self) -> Result<LiveState> {
        let mut conn = crate::read::ReadSession::application(self.connector.clone(), Scope::Server);
        let now = query_one(&mut conn, "SELECT DATE_FORMAT(UTC_TIMESTAMP(), '%Y-%m-%dT%H:%i:%sZ') AS now")
            .await?
            .map(|row| row.string("now"))
            .unwrap_or_default();
        let flavor = self.connector.flavor();
        // MariaDB's process list also has the time in milliseconds and progress for ALTER TABLE, LOAD DATA and others.
        let extra = match flavor {
            Flavor::Mariadb => "p.TIME_MS AS ms, p.PROGRESS AS progress, p.STAGE AS stage, p.MAX_STAGE AS max_stage",
            Flavor::Mysql => "NULL AS ms, NULL AS progress, NULL AS stage, NULL AS max_stage",
        };
        let rows = query(
            &mut conn,
            &format!(
                "SELECT p.ID AS id, p.USER AS user_name, p.HOST AS host, p.DB AS db, p.COMMAND AS command, p.TIME AS seconds,
                        p.STATE AS state, p.INFO AS info, {extra},
                        TIMESTAMPDIFF(MICROSECOND, t.trx_started, NOW(6)) / 1000 AS trx_ms
                   FROM information_schema.PROCESSLIST p
                   LEFT JOIN information_schema.INNODB_TRX t ON t.trx_mysql_thread_id = p.ID
                  WHERE p.ID <> CONNECTION_ID() AND p.COMMAND NOT IN ('Daemon', 'Binlog Dump', 'Binlog Dump GTID')
                    AND p.USER <> 'system user'"
            ),
        )
        .await?;
        let deadlocks::BlockingEvidence { blockers, locks, resources, mut notes } =
            deadlocks::blocking_evidence(&mut conn, flavor).await;
        let last_statements = deadlocks::last_statements(&mut conn).await;
        conn.close().await;

        let sessions: Vec<Session> = rows
            .iter()
            .filter(|r| {
                !r.text("info").is_some_and(|i| {
                    culprit_core::telemetry::StatementOrigin::from_sql(Some(&i), Engine::Mysql).0.is_culprit()
                })
            })
            .map(|r| {
                let id = r.string("id");
                let command = r.string("command");
                let in_transaction = r.number("trx_ms").is_some();
                let state = match command.as_str() {
                    "Sleep" if in_transaction => "idle in transaction".to_string(),
                    "Sleep" => "idle".to_string(),
                    _ => "active".to_string(),
                };
                let active = state == "active";
                let wait = if active { r.text("state").filter(|s| !s.is_empty()) } else { None };
                Session {
                    blocked_by: blockers.get(&id).cloned(),
                    user: r.text("user_name"),
                    application: None,
                    host: r.text("host"),
                    database: r.text("db"),
                    command: active.then(|| command.to_uppercase()),
                    waiting_on_lock: native::activity::waiting_on_lock(wait.as_deref(), blockers.contains_key(&id)),
                    wait: wait.map(|w| with_progress(w, r)),
                    wait_ms: None,
                    wait_resources: resources.get(&id).cloned(),
                    elapsed_ms: r.number("ms").or_else(|| r.number("seconds").map(|s| s * 1000.0)),
                    transaction_ms: r.number("trx_ms"),
                    cpu_ms: None,
                    reads: None,
                    // An idle session in a transaction shows its last statement: usually what took the locks.
                    sql: r
                        .text("info")
                        .filter(|s| !s.is_empty())
                        .or_else(|| in_transaction.then(|| last_statements.get(&id).cloned()).flatten()),
                    state,
                    id,
                }
            })
            .collect();
        let mut waits: Vec<WaitStat> = Vec::new();
        for session in sessions.iter().filter(|s| s.state == "active") {
            let waiting = session.wait.as_deref().unwrap_or("").to_ascii_lowercase();
            let category = if session.waiting_on_lock == Some(true) {
                "Lock"
            } else if waiting.starts_with("waiting") {
                "Waiting"
            } else {
                "Running"
            }
            .to_string();
            match waits.iter_mut().find(|w| w.category == category) {
                Some(existing) => existing.tasks = Some(existing.tasks.unwrap_or(0.0) + 1.0),
                None => waits.push(WaitStat { category, wait_type: None, wait_ms: 0.0, tasks: Some(1.0) }),
            }
        }
        if sessions.iter().any(|s| {
            s.blocked_by.is_none()
                && s.wait.as_deref().is_some_and(|w| w.to_ascii_lowercase().contains("metadata lock"))
        }) {
            notes.push(unresolved_metadata_lock());
        }
        let blocking = culprit_core::blocking::analyze(Engine::Mysql, &sessions, &locks, &[]);
        Ok(LiveState {
            notes,
            taken_at: now,
            sessions,
            waits,
            session_actions: Vec::new(),
            session_actions_note: None,
            blocking,
        })
    }

    async fn run(&self, sql: &str, limit: usize) -> Result<ResultSet> {
        self.read(sql, limit).await
    }

    async fn preview(&self, sql: &str, limit: usize) -> Result<ResultSet> {
        preview::read(self.connector.clone(), single_statement(sql)?.to_owned(), limit).await
    }

    async fn execute(&self, sql: &str, limit: usize) -> Result<ResultSet> {
        let statement = single_statement(sql)?;
        let mut conn = self.connector.connect(Scope::Database).await?;
        let started = Instant::now();
        // Sessions open read-only; a write the person chose to run gets a read-write session and commits (autocommit).
        conn.query_drop(tagged("SET SESSION TRANSACTION READ WRITE")).await.map_err(map_error)?;
        // MySQL's timeout only stops SELECTs; MariaDB's stops anything, so it is lifted for a change the person ran.
        if let Some(lift) = self.connector.flavor().no_timeout() {
            conn.query_drop(tagged(lift)).await.map_err(map_error)?;
        }
        let result = run_in_transaction(&mut conn, statement, limit, true).await;
        let _ = conn.disconnect().await;
        let mut result = result?;
        result.elapsed_ms = started.elapsed().as_secs_f64() * 1000.0;
        Ok(result)
    }
}

/// A process-list state with MariaDB's progress report, when the statement has one (ALTER TABLE, LOAD DATA, …).
fn with_progress(state: String, r: &rows::Rec) -> String {
    progress_label(state, r.number("progress"), r.number("stage"), r.number("max_stage"))
}

fn progress_label(state: String, progress: Option<f64>, stage: Option<f64>, max: Option<f64>) -> String {
    let Some(progress) = progress.filter(|p| *p > 0.0) else { return state };
    let percent = format::digits(format!("{progress:.0}"));
    match (stage, max) {
        (Some(stage), Some(max)) if max > 1.0 => t!(
            "provider.mysql.live.progress_stage",
            state = state,
            stage = format::digits(format!("{stage:.0}")),
            stages = format::digits(format!("{max:.0}")),
            percent = percent
        ),
        _ => t!("provider.mysql.live.progress", state = state, percent = percent),
    }
}

fn not_mysql(engine: Engine) -> Error {
    Error::invalid(t!("provider.mysql.not_mysql", engine = engine.to_string()))
}

/// A statement EXPLAIN doesn't take (`verb` lowercased).
fn cant_explain(flavor: Flavor, verb: &str) -> Error {
    Error::new(
        culprit_core::ErrorCode::Unsupported,
        t!("provider.mysql.explain.cant_explain", server = flavor.name(), verb = verb.to_ascii_uppercase()),
    )
    .with_hint(t!("provider.mysql.explain.cant_explain_hint"))
}

/// A write (`verb` lowercased) the server refused to plan in Culprit's read-only session; `values`: an INSERT … VALUES,
/// which searches nothing.
fn no_plan_for_write(flavor: Flavor, verb: &str, values: bool) -> Error {
    Error::new(
        culprit_core::ErrorCode::Unsupported,
        t!("provider.mysql.explain.read_only_write", server = flavor.name(), verb = verb.to_ascii_uppercase()),
    )
    .with_hint(if values {
        t!("provider.mysql.explain.insert_values_hint")
    } else {
        t!("provider.mysql.explain.explain_the_select_hint")
    })
}

/// The command that returned no plan.
#[derive(Clone, Copy, Debug)]
enum PlanCommand {
    Explain,
    Analyze,
    ExplainAnalyze,
}

fn no_plan(command: PlanCommand) -> Error {
    Error::internal(match command {
        PlanCommand::Explain => t!("provider.mysql.explain.no_plan.explain"),
        PlanCommand::Analyze => t!("provider.mysql.explain.no_plan.analyze"),
        PlanCommand::ExplainAnalyze => t!("provider.mysql.explain.no_plan.explain_analyze"),
    })
}

/// MySQL before 8.0.18 (`version` as the server reports it) has no EXPLAIN ANALYZE.
fn cant_measure(version: &str) -> Error {
    let short = version.split('-').next().unwrap_or(version);
    Error::new(culprit_core::ErrorCode::Unsupported, t!("provider.mysql.explain.cant_measure", version = short))
        .with_hint(t!("provider.mysql.explain.cant_measure_hint"))
}

fn analyze_reads_only() -> Error {
    Error::invalid(t!("provider.mysql.explain.analyze_reads_only"))
        .with_hint(t!("provider.mysql.explain.analyze_reads_only_hint"))
}

/// A recorded statement whose placeholders stop it from being planned.
fn recorded_without_values(flavor: Flavor) -> Error {
    Error::invalid(t!("provider.mysql.explain.recorded_without_values", server = flavor.name()))
        .with_hint(t!("provider.mysql.explain.recorded_without_values_hint"))
}

fn unresolved_metadata_lock() -> String {
    t!("provider.mysql.live.unresolved_metadata_lock")
}

/// What running a statement says: how many rows it returned (`notice`: why Culprit stopped reading them), or, for a
/// change the person ran (`committed`), how many it changed; a change that returned rows says it committed.
fn result_messages(count: u64, notice: Option<String>, columns: bool, committed: bool) -> Vec<String> {
    let rows = format::digits(count.to_string());
    if committed && !columns {
        // Nothing was read back, so nothing was cut short: there is no notice to give.
        return vec![t!("provider.mysql.result.rows_changed", n = count, count = rows)];
    }
    let mut messages = vec![match notice {
        Some(notice) => t!("provider.mysql.result.rows_stopped", n = count, count = rows, notice = notice),
        None => t!("provider.mysql.result.rows", n = count, count = rows),
    }];
    if committed {
        messages.push(t!("provider.mysql.result.committed"));
    }
    messages
}

/// Trim and reject empty or multi-statement input (the server would refuse the second statement anyway).
/// One statement, checked by Culprit's lexer under both backslash modes (and refusing executable comments).
fn single_statement(sql: &str) -> Result<&str> {
    culprit_core::sql::single_statement(sql, Engine::Mysql)
}

fn reads_only(statement: &str) -> bool {
    culprit_core::sql::reads_only(statement, Engine::Mysql)
}

/// `committed`: a change the person ran, which commits as it finishes (autocommit).
async fn run_in_transaction(conn: &mut Conn, statement: &str, limit: usize, committed: bool) -> Result<ResultSet> {
    read_result(conn, &tagged(statement), limit, committed).await.map_err(map_error)
}

async fn read_result(
    conn: &mut Conn,
    tagged_statement: &str,
    limit: usize,
    committed: bool,
) -> mysql_async::Result<ResultSet> {
    read_result_tracked(conn, tagged_statement, limit, committed, &culprit_core::telemetry::ReadProgress::default())
        .await
}

async fn read_result_tracked(
    conn: &mut Conn,
    tagged_statement: &str,
    limit: usize,
    committed: bool,
    progress: &culprit_core::telemetry::ReadProgress,
) -> mysql_async::Result<ResultSet> {
    let mut result = conn.query_iter(tagged_statement).await?;
    let columns_meta = result.columns();
    let columns: Vec<String> = columns_meta
        .as_deref()
        .map(|cols| cols.iter().map(|c| c.name_str().into_owned()).collect())
        .unwrap_or_default();
    let types: Vec<ColumnType> =
        columns_meta.as_deref().map(|cols| cols.iter().map(|c| c.column_type()).collect()).unwrap_or_default();
    let mut rows: Vec<Vec<Json>> = Vec::new();
    let mut budget = culprit_core::limits::ResultBudget::new(limit);
    while let Some(row) = result.next().await? {
        progress.rows.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let values = row.unwrap();
        let charged = rows::decoded_value_bytes(&values);
        progress.bytes.fetch_add(charged as u64, std::sync::atomic::Ordering::Relaxed);
        if budget.truncated() {
            continue;
        }
        let bytes = values.iter().fold(0usize, |n, v| {
            n.saturating_add(match v {
                Value::Bytes(b) => culprit_core::limits::text_bytes(b),
                _ => 64,
            })
        });
        if !budget.retain(culprit_core::limits::row_bytes(values.len(), bytes)) {
            continue;
        }
        rows.push(values.iter().enumerate().map(|(i, v)| json_value(v, types.get(i))).collect());
    }
    let affected = result.affected_rows();
    result.drop_result().await?;
    progress.drained.store(true, std::sync::atomic::Ordering::Release);
    let count = if columns.is_empty() { affected } else { rows.len() as u64 };
    let messages = result_messages(count, budget.notice(), !columns.is_empty(), committed);
    Ok(ResultSet { columns, rows, truncated: budget.truncated(), elapsed_ms: 0.0, messages })
}

fn json_value(value: &Value, kind: Option<&ColumnType>) -> Json {
    match value {
        Value::NULL => Json::Null,
        Value::Int(i) => Json::from(*i),
        Value::UInt(u) => Json::from(*u),
        Value::Float(f) => serde_json::Number::from_f64(f64::from(*f)).map(Json::Number).unwrap_or(Json::Null),
        Value::Double(d) => serde_json::Number::from_f64(*d).map(Json::Number).unwrap_or(Json::Null),
        Value::Bytes(bytes) => {
            let text = String::from_utf8_lossy(bytes).into_owned();
            match kind {
                Some(
                    ColumnType::MYSQL_TYPE_TINY
                    | ColumnType::MYSQL_TYPE_SHORT
                    | ColumnType::MYSQL_TYPE_LONG
                    | ColumnType::MYSQL_TYPE_LONGLONG
                    | ColumnType::MYSQL_TYPE_INT24
                    | ColumnType::MYSQL_TYPE_YEAR,
                ) => text.parse::<i64>().map(Json::from).unwrap_or(Json::String(text)),
                Some(ColumnType::MYSQL_TYPE_FLOAT | ColumnType::MYSQL_TYPE_DOUBLE) => text
                    .parse::<f64>()
                    .ok()
                    .and_then(serde_json::Number::from_f64)
                    .map(Json::Number)
                    .unwrap_or(Json::String(text)),
                _ => Json::String(text),
            }
        }
        other => Json::String(value_text(other)),
    }
}

/// An `INSERT`/`REPLACE … VALUES` (no SELECT behind it): nothing to search, so nothing to plan.
fn lower_starts_values(statement: &str) -> bool {
    let lower = statement.to_ascii_lowercase();
    matches!(leading_verb(statement).as_str(), "insert" | "replace")
        && (lower.contains(" values") || lower.contains(" value ") || lower.contains(" set "))
        && !lower.contains("select")
}

/// What MySQL and MariaDB can EXPLAIN (`(`, `with`, `table`, `values` start reads too).
const EXPLAINABLE: [&str; 9] = ["select", "with", "table", "values", "(", "insert", "update", "delete", "replace"];
const WRITES: [&str; 4] = ["insert", "update", "delete", "replace"];

/// The statement's first word after any comments, lowercased (`(` for a parenthesized read).
fn leading_verb(statement: &str) -> String {
    let mut text = statement.trim_start();
    loop {
        if let Some(rest) = text.strip_prefix("/*") {
            text = rest.split_once("*/").map_or("", |(_, after)| after).trim_start();
        } else if text.starts_with("-- ") || text.starts_with('#') {
            text = text.split_once(['\n', '\r']).map_or("", |(_, after)| after).trim_start();
        } else {
            break;
        }
    }
    if text.starts_with('(') {
        return "(".into();
    }
    text.chars().take_while(|c| c.is_ascii_alphabetic()).collect::<String>().to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_leading_verb_skips_comments() {
        assert_eq!(super::leading_verb("/* app */ SELECT 1"), "select");
        assert_eq!(super::leading_verb("-- note\nUPDATE t SET a = 1"), "update");
        assert_eq!(super::leading_verb("# note\r\n DO `SLEEP` (?)"), "do");
        assert_eq!(super::leading_verb("(SELECT 1) UNION (SELECT 2)"), "(");
    }

    use super::*;

    #[test]
    fn placeholders_in_explain_get_a_plain_hint() {
        let raw = Error::new(
            culprit_core::ErrorCode::QueryFailed,
            "Driver error: `Statement takes 1 parameters but 0 was supplied.'",
        );
        let mapped = parameter_error(raw);
        assert!(mapped.message.contains("`?` parameters"), "{}", mapped.message);
        assert!(mapped.hint.as_deref().is_some_and(|h| h.contains("sample value")));
        assert_eq!(parameter_error(Error::invalid("other")).message, "other");
    }
    use culprit_core::ErrorCode;

    #[test]
    fn values_follow_column_types() {
        assert_eq!(json_value(&Value::Bytes(b"42".to_vec()), Some(&ColumnType::MYSQL_TYPE_LONGLONG)), Json::from(42));
        assert_eq!(json_value(&Value::Bytes(b"1.5".to_vec()), Some(&ColumnType::MYSQL_TYPE_DOUBLE)), Json::from(1.5));
        assert_eq!(
            json_value(&Value::Bytes(b"1.50".to_vec()), Some(&ColumnType::MYSQL_TYPE_NEWDECIMAL)),
            Json::String("1.50".into())
        );
        assert_eq!(json_value(&Value::NULL, None), Json::Null);
    }

    #[test]
    fn only_reads_are_analyzed() {
        assert!(reads_only("select 1"));
        assert!(reads_only(" WITH x AS (SELECT 1) SELECT * FROM x"));
        assert!(!reads_only("DELETE FROM orders"));
        assert_eq!(single_statement("  select 1 ;  ").unwrap(), "select 1");
        assert!(single_statement(" ; ").is_err());
    }

    #[test]
    fn rejects_other_engines() {
        let spec = ConnectionSpec { engine: Some(Engine::Postgres), host: "h".into(), ..Default::default() };
        assert_eq!(MysqlProvider::new(&spec).err().unwrap().code, ErrorCode::InvalidInput);
    }

    /// An error's message and hint, as a person reads them.
    fn said(e: Error) -> (String, Option<String>) {
        (e.message, e.hint)
    }

    // What explaining, running and watching a statement say, word for word: its messages come from locales/en.json.
    #[test]
    fn explain_errors_say_why_in_english() {
        let spec = ConnectionSpec { engine: Some(Engine::Postgres), host: "h".into(), ..Default::default() };
        assert_eq!(MysqlProvider::new(&spec).err().unwrap().message, "This connection is for PostgreSQL, not MySQL");
        assert_eq!(
            said(cant_explain(Flavor::Mariadb, "do")),
            (
                "MariaDB can't EXPLAIN a DO statement".into(),
                Some(
                    "EXPLAIN covers SELECT, INSERT, UPDATE, DELETE and REPLACE. The scan's statement statistics show \
                     this one's measured times."
                        .into()
                )
            )
        );
        assert_eq!(
            said(no_plan_for_write(Flavor::Mysql, "insert", true)),
            (
                "MySQL won't plan an INSERT in a read-only session, and Culprit's sessions are always read-only".into(),
                Some(
                    "An INSERT … VALUES searches no rows, so there's nothing to plan; its cost is the indexes and \
                     triggers on the table."
                        .into()
                )
            )
        );
        assert_eq!(
            said(no_plan_for_write(Flavor::Mariadb, "update", false)),
            (
                "MariaDB won't plan an UPDATE in a read-only session, and Culprit's sessions are always read-only"
                    .into(),
                Some(
                    "Explain the SELECT with the same FROM and WHERE: the server usually finds the rows the same way."
                        .into()
                )
            )
        );
        assert_eq!(
            no_plan_for_write(Flavor::Mysql, "delete", false).message,
            "MySQL won't plan a DELETE in a read-only session, and Culprit's sessions are always read-only"
        );
        assert_eq!(
            no_plan_for_write(Flavor::Mysql, "replace", true).message,
            "MySQL won't plan a REPLACE in a read-only session, and Culprit's sessions are always read-only"
        );
        assert_eq!(no_plan(PlanCommand::Explain).message, "EXPLAIN returned no plan");
        assert_eq!(no_plan(PlanCommand::Analyze).message, "ANALYZE returned no plan");
        assert_eq!(no_plan(PlanCommand::ExplainAnalyze).message, "EXPLAIN ANALYZE returned no plan");
        assert_eq!(
            said(cant_measure("8.0.17-log")),
            (
                "MySQL 8.0.17 can't measure a run: EXPLAIN ANALYZE arrived in MySQL 8.0.18".into(),
                Some(
                    "Explain shows the estimated plan; the scan's statement statistics show each query's measured times."
                        .into()
                )
            )
        );
        assert_eq!(
            said(analyze_reads_only()),
            (
                "Explain Analyze runs the statement, so Culprit only allows it for SELECT".into(),
                Some("Use Explain (estimated) for statements that change data.".into())
            )
        );
        let raw =
            Error::new(ErrorCode::QueryFailed, "Driver error: `Statement takes 1 parameters but 0 was supplied.'");
        assert_eq!(
            said(parameter_error(raw)),
            (
                "This statement has `?` parameters, which MySQL needs values for to plan it".into(),
                Some(
                    "Replace each ? with a sample value of its type (a quoted string for text, a number for an id) and \
                     explain again. Queries Culprit captured open from Queries with the plan of a real statement."
                        .into()
                )
            )
        );
        for (flavor, name) in [(Flavor::Mysql, "MySQL"), (Flavor::Mariadb, "MariaDB")] {
            assert_eq!(
                said(recorded_without_values(flavor)),
                (
                    format!(
                        "{name} recorded this statement without its values (each ? stands for one), so it can't be \
                         planned as recorded"
                    ),
                    Some("Open it in the editor, put a real value in place of each ?, and explain it there.".into())
                )
            );
        }
    }

    #[test]
    fn results_and_sessions_say_what_happened_in_english() {
        assert_eq!(result_messages(1, None, true, false), ["1 row"]);
        assert_eq!(result_messages(1500, None, true, false), ["1500 rows"]);
        assert_eq!(result_messages(0, None, true, false), ["0 rows"]);
        assert_eq!(result_messages(1000, Some("stopped at 1000".into()), true, false), ["1000 rows (stopped at 1000)"]);
        assert_eq!(result_messages(3, None, false, true), ["3 rows changed, committed."]);
        assert_eq!(result_messages(1, None, false, true), ["1 row changed, committed."]);
        assert_eq!(result_messages(2, None, true, true), ["2 rows", "Committed."]);
        assert_eq!(result_messages(0, None, false, false), ["0 rows"]);
        assert_eq!(progress_label("altering table".into(), None, None, None), "altering table");
        assert_eq!(progress_label("altering table".into(), Some(0.0), Some(1.0), Some(2.0)), "altering table");
        assert_eq!(
            progress_label("altering table".into(), Some(42.4), Some(1.0), Some(2.0)),
            "altering table · stage 1 of 2, 42%"
        );
        assert_eq!(
            progress_label("copy to tmp table".into(), Some(7.6), Some(1.0), Some(1.0)),
            "copy to tmp table · 8%"
        );
        assert_eq!(progress_label("copy to tmp table".into(), Some(50.0), None, None), "copy to tmp table · 50%");
        assert_eq!(
            unresolved_metadata_lock(),
            "A metadata-lock wait is visible but its holder could not be resolved. Check sys.schema_table_lock_waits, \
             metadata-lock instrumentation and this login's visibility; an unresolved blocker is not an unblocked \
             session."
        );
    }
}
