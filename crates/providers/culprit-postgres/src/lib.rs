//! Postgres provider for Culprit: read-only snapshots from the statistics views, plans via EXPLAIN, live sessions.

mod catalog;
mod collect;
mod connect;
pub mod deadlocks;
mod generic;
mod memory;
mod native;
mod operations;
pub mod plan_json;
mod pressure;
mod probe;
mod progress;
mod read;
mod recorded;
mod results;
mod rows;
mod sessions;
mod signals;
pub mod sources;
pub mod structured_logs;
mod tables;
mod telemetry;
mod timescale;
mod trust;
pub mod wire;
mod workload;

use std::time::Instant;

use async_trait::async_trait;
use culprit_core::{
    ConnectionSpec, Engine, Environment, Error, ExplainMode, LiveState, Plan, PlanKind, Provider, Result, ResultSet,
    ServerInfo, Session, SessionAction, Snapshot, SnapshotOptions, WaitStat, t,
};
use serde_json::Value;
use tokio_postgres::Client;
use tokio_postgres::types::Type;

use connect::{COLLECTOR, Connector, EXECUTION, INTERACTIVE, map_error, sql_state};
use rows::{query, query_one, tagged};
pub use workload::{leading_keyword, plannable};

const CURSOR: &str = "culprit_cursor";

/// Identity of this crate's collector and native-plan parser, including a build revision when supplied.
pub fn component_version() -> culprit_core::ComponentVersion {
    culprit_core::ComponentVersion::package(
        "culprit-postgres",
        env!("CARGO_PKG_VERSION"),
        option_env!("CULPRIT_BUILD_REVISION").or(option_env!("GITHUB_SHA")),
    )
}

pub struct PostgresProvider {
    connector: Connector,
    spec: ConnectionSpec,
}

impl PostgresProvider {
    pub fn new(spec: &ConnectionSpec) -> Result<Self> {
        if let Some(engine) = spec.engine
            && engine != Engine::Postgres
        {
            return Err(Error::invalid(t!("provider.postgres.error.other_engine", engine = engine.to_string())));
        }
        Ok(PostgresProvider { connector: Connector::new(spec)?, spec: spec.clone() })
    }

    async fn read_with_limits(&self, sql: &str, limit: usize, limits: connect::SessionLimits) -> Result<ResultSet> {
        let statement = single_statement(sql)?;
        let client = self.connector.connect(limits).await?;
        let started = Instant::now();
        client.batch_execute(&tagged("BEGIN READ ONLY")).await.map_err(map_error)?;
        let result = run_in_transaction(&client, statement, limit, true).await;
        let rollback = client.batch_execute(&tagged("ROLLBACK")).await;
        let mut result = result?;
        rollback.map_err(map_error)?;
        result.elapsed_ms = started.elapsed().as_secs_f64() * 1000.0;
        Ok(result)
    }

    async fn hypothetical_wire(&self, sql: &str, indexes: &[String], fill_table_sizes: bool) -> Result<Plan> {
        let statement = single_statement(sql)?;
        let client = self.connector.connect(INTERACTIVE).await?;
        if !fill_table_sizes
            && query_one(
                &client,
                "SELECT backend_type FROM pg_catalog.pg_stat_activity WHERE pid=pg_catalog.pg_backend_pid()",
            )
            .await?
            .is_none_or(|row| row.text("backend_type").as_deref() != Some("client backend"))
        {
            return Err(Error::new(
                culprit_core::ErrorCode::Unsupported,
                t!("provider.yugabyte.error.pooled_virtual_indexes"),
            )
            .with_hint(t!("provider.yugabyte.error.pooled_virtual_indexes_hint")));
        }
        let hypopg_schema = query_one(
            &client,
            "SELECT n.nspname AS schema FROM pg_catalog.pg_extension e JOIN pg_catalog.pg_namespace n ON n.oid = e.extnamespace WHERE e.extname = 'hypopg'",
        )
        .await?
        .map(|r| r.string("schema"));
        let Some(ext_schema) = hypopg_schema else {
            return Err(Error::new(culprit_core::ErrorCode::Unsupported, t!("provider.postgres.error.no_hypopg"))
                .with_hint(t!("provider.postgres.error.no_hypopg_hint")));
        };
        let q_schema = Engine::Postgres.dialect().quote(&ext_schema);
        let reset_sql = format!("SELECT {q_schema}.hypopg_reset()");
        let create_sql = format!("SELECT * FROM {q_schema}.hypopg_create_index($1)");
        let mut cleanup_needed = false;
        let result = async {
            let elevated = trust::elevated(&client).await?;
            // Registration can reach expression/operator code too. Apply the planner trust and read-only
            // boundary before invoking HypoPG; virtual indexes live in session memory across this rollback.
            trust::begin(&client, elevated).await?;
            cleanup_needed = true;
            let registered = async {
                client.batch_execute(&tagged(&reset_sql)).await.map_err(map_error)?;
                let mut virtual_names = Vec::new();
                for raw_idx in indexes {
                    let idx_stmt = single_statement(raw_idx)?;
                    if !culprit_core::sql::creates_index(idx_stmt, Engine::Postgres) {
                        return Err(Error::invalid(t!("provider.postgres.error.not_create_index")));
                    }
                    let cleaned = strip_concurrently_keyword(idx_stmt);
                    let rows = client.query(&tagged(&create_sql), &[&cleaned]).await.map_err(map_error)?;
                    if rows.is_empty() {
                        return Err(Error::invalid(t!(
                            "provider.postgres.error.no_virtual_index",
                            statement = idx_stmt
                        )));
                    }
                    for row in rows {
                        if let Ok(name) = row.try_get::<_, String>("indexname").or_else(|_| row.try_get::<_, String>(1))
                        {
                            virtual_names.push(name);
                        }
                    }
                }
                Ok(virtual_names)
            }
            .await;
            trust::end(&client).await;
            let virtual_names = registered?;
            let parameters = parameter_count(&client, statement).await?;
            let mut plan = if parameters == 0 {
                trust::begin(&client, elevated).await?;
                let explained = explain_text(
                    &client,
                    &format!("EXPLAIN (FORMAT JSON, VERBOSE) {statement}"),
                    PlanKind::Estimated,
                    statement,
                )
                .await;
                trust::end(&client).await;
                explained?
            } else {
                let json = generic::plan_json(&client, statement, server_version(&client).await?, elevated).await?;
                plan_json::parse(&json, PlanKind::Generic, Some(statement))?
            };
            if fill_table_sizes {
                tables::fill(&client, &mut plan).await;
            }
            if !virtual_names.is_empty() {
                plan.facts_mut().virtual_indexes = virtual_names.clone();
                plan.props.insert("VirtualIndexes".into(), virtual_names.join(", "));
            }
            Ok(plan)
        }
        .await;
        if cleanup_needed {
            let _ = client.batch_execute(&tagged(&reset_sql)).await;
        }
        result
    }

    fn environment(&self) -> Environment {
        self.spec.environment
    }

    async fn explain_wire(
        &self,
        sql: &str,
        mode: ExplainMode,
        fill_table_sizes: bool,
        distributed: bool,
    ) -> Result<Plan> {
        let statement = single_statement(sql)?;
        match mode {
            ExplainMode::Estimated => {
                let client = self.connector.connect(INTERACTIVE).await?;
                let parameters = parameter_count(&client, statement).await?;
                // Planning runs the code a statement reaches, with this login's privileges (`trust`).
                let elevated = trust::elevated(&client).await?;
                let mut plan = if parameters == 0 {
                    trust::begin(&client, elevated).await?;
                    let explained = explain_text(
                        &client,
                        &format!("EXPLAIN (FORMAT JSON, VERBOSE) {statement}"),
                        PlanKind::Estimated,
                        statement,
                    )
                    .await;
                    trust::end(&client).await;
                    explained?
                } else {
                    // Placeholders ($1, $2… as pg_stat_statements records statements): the generic plan, never run.
                    let json = generic::plan_json(&client, statement, server_version(&client).await?, elevated).await?;
                    plan_json::parse(&json, PlanKind::Generic, Some(statement))?
                };
                if fill_table_sizes {
                    tables::fill(&client, &mut plan).await;
                }
                Ok(plan)
            }
            ExplainMode::Analyze => {
                let client = self.connector.connect(EXECUTION).await?;
                let begin = if self.environment().is_production() { "BEGIN READ ONLY" } else { "BEGIN READ WRITE" };
                client.batch_execute(&tagged(begin)).await.map_err(map_error)?;
                let result = async {
                    if parameter_count(&client, statement).await? > 0 {
                        return Err(Error::invalid(t!("provider.postgres.error.analyze_parameters")));
                    }
                    explain_text(
                        &client,
                        &format!(
                            "EXPLAIN (ANALYZE, {}, VERBOSE, FORMAT JSON) {statement}",
                            if distributed { "DIST" } else { "BUFFERS" }
                        ),
                        PlanKind::Actual,
                        statement,
                    )
                    .await
                }
                .await;
                let rollback = client.batch_execute(&tagged("ROLLBACK")).await;
                let mut plan = result?;
                rollback.map_err(map_error)?;
                plan.facts_mut().execution_protection =
                    Some(culprit_core::plan_facts::ExecutionProtection::RolledBackTransaction);
                Ok(plan)
            }
        }
    }
}

#[async_trait]
impl Provider for PostgresProvider {
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
        telemetry::resources(self.connector.clone(), request).await
    }

    fn collector_version(&self) -> culprit_core::ComponentVersion {
        component_version()
    }

    fn engine(&self) -> Engine {
        Engine::Postgres
    }

    async fn probe(&self) -> Result<ServerInfo> {
        let client = self.connector.connect(COLLECTOR).await?;
        let probe = probe::probe(&client, &self.connector.host).await?;
        Ok(probe.server_info())
    }

    async fn snapshot(&self, connection: &str, options: &SnapshotOptions) -> Result<Snapshot> {
        collect::snapshot(&self.connector, &self.spec, connection, options).await
    }

    async fn explain(&self, sql: &str, mode: ExplainMode) -> Result<Plan> {
        self.explain_wire(sql, mode, true, false).await
    }

    async fn interrupt_session(&self, session: &str, action: SessionAction) -> Result<()> {
        sessions::interrupt(&self.connector, session, action).await
    }

    async fn lock_waits(&self) -> Result<Vec<culprit_core::LockWait>> {
        let client = self.connector.connect(INTERACTIVE).await?;
        deadlocks::lock_waits(&client).await
    }

    async fn deadlocks(&self, since_hours: f64) -> Result<culprit_core::DeadlockReport> {
        let client = self.connector.connect(INTERACTIVE).await?;
        deadlocks::recent(&client, since_hours).await
    }

    async fn recorded_changes(&self, since: &str) -> Result<Vec<culprit_core::ChangeEvent>> {
        let client = self.connector.connect(INTERACTIVE).await?;
        recorded::changes(&client, since).await
    }

    async fn wait_sample(&self, seconds: u32) -> Result<culprit_core::WaitSample> {
        let client = self.connector.connect(INTERACTIVE).await?;
        crate::pressure::sample(&client, seconds).await
    }

    async fn explain_recorded(&self, sql: &str, query_id: Option<&str>) -> Result<Plan> {
        // Someone else's text: an elevated login plans only its own copy of it (`trust`), anyone else plans it as is.
        let client = self.connector.connect(INTERACTIVE).await?;
        if !trust::elevated(&client).await? {
            return self.explain(sql, ExplainMode::Estimated).await;
        }
        let own = match query_id {
            Some(id) => trust::own_text(&client, id).await?,
            None => None,
        };
        match own {
            Some(text) => self.explain(&text, ExplainMode::Estimated).await,
            None => Err(trust::not_its_own()),
        }
    }

    async fn explain_hypothetical(&self, sql: &str, indexes: &[String]) -> Result<Plan> {
        self.hypothetical_wire(sql, indexes, true).await
    }

    async fn query_progress(&self, session: &str, with_plan: bool) -> Result<culprit_core::QueryProgress> {
        let mut progress = progress::query_progress(&self.connector, session).await?;
        if with_plan
            && progress.running
            && let Some(statement) = progress.statement.clone()
        {
            // EXPLAIN without ANALYZE: plans the statement's text again, never runs it. Another session's statement,
            // with its real values, is another role's text unless that session is this login's: an elevated login
            // plans only its own (`trust`).
            let own_or_plain = async {
                let client = self.connector.connect(INTERACTIVE).await?;
                if !trust::elevated(&client).await? {
                    return Ok(true);
                }
                let pid: i32 = session
                    .trim()
                    .parse()
                    .map_err(|_| Error::invalid(t!("provider.postgres.error.session_not_pid")))?;
                trust::session_is_own(&client, pid).await
            };
            let planned = match own_or_plain.await {
                Ok(true) => self.explain(&statement, ExplainMode::Estimated).await,
                Ok(false) => Err(trust::not_its_own()),
                Err(e) => Err(e),
            };
            match planned {
                Ok(mut plan) => {
                    plan.raw = None;
                    progress.plan = Some(plan);
                }
                Err(e) => {
                    let base = progress.note.take().unwrap_or_default();
                    let failed = t!("provider.postgres.progress.plan_failed", error = &e.message);
                    progress.note = Some(format!("{base} {failed}").trim().to_string());
                }
            }
        }
        Ok(progress)
    }

    async fn live(&self) -> Result<LiveState> {
        use crate::read::NativeRead;
        let budget = culprit_core::telemetry::CollectionBudget {
            total_timeout_ms: culprit_core::limits::OBSERVE_TIMEOUT.as_millis() as u32,
            ..Default::default()
        };
        let mut reader = crate::read::ReadSession::new(self.connector.clone(), budget.clone());
        let result = async {
            let meta = crate::native::metadata::read(&mut reader, &self.connector.host, self.connector.port).await?;
            let mut frame = crate::native::activity::observe(&mut reader, &meta, &budget).await?;
            crate::native::activity::blockers(
                &mut reader,
                &mut frame,
                crate::native::activity::BlockingReadOptions {
                    minimum_request_age_ms: 0,
                    scope: crate::native::activity::ActivityScope::Visible,
                    wait_start_available: meta.version >= 140_000,
                },
            )
            .await?;
            let keys = frame.observations().into_iter().map(|row| row.key).collect();
            let cap = (budget.page_bytes as usize / frame.records.len().max(1))
                .saturating_sub(1024)
                .min(budget.max_sql_bytes as usize);
            crate::native::activity::enrich(&mut reader, &mut frame, &keys, cap, true).await?;
            let now = frame.at.clone();
            let sessions: Vec<Session> = frame
                .records
                .iter()
                .map(crate::native::activity::project)
                .filter(|row| row.application.as_deref() != Some("culprit"))
                .collect();
            let mut waits: Vec<WaitStat> = Vec::new();
            for session in sessions.iter().filter(|s| s.state == "active") {
                let category = session.wait.as_deref().and_then(|w| w.split(':').next()).unwrap_or("CPU").to_string();
                let wait_type = session.wait.as_deref().and_then(|w| w.split_once(": ")).map(|(_, e)| e.to_string());
                match waits.iter_mut().find(|w| w.category == category && w.wait_type == wait_type) {
                    Some(existing) => existing.tasks = Some(existing.tasks.unwrap_or(0.0) + 1.0),
                    None => waits.push(WaitStat { category, wait_type, wait_ms: 0.0, tasks: Some(1.0) }),
                }
            }
            waits.sort_by(|a, b| b.tasks.unwrap_or(0.0).total_cmp(&a.tasks.unwrap_or(0.0)));
            let blocking = culprit_core::blocking::analyze(Engine::Postgres, &sessions, &frame.locks);
            Ok(LiveState {
                notes: Vec::new(),
                taken_at: now,
                sessions,
                waits,
                session_actions: Vec::new(),
                session_actions_note: None,
                blocking,
            })
        }
        .await;
        if reader.close().await == culprit_core::telemetry::SourceShutdown::Unconfirmed {
            return Err(culprit_core::telemetry::CollectionStop::CancellationUnconfirmed.error());
        }
        result
    }

    async fn run(&self, sql: &str, limit: usize) -> Result<ResultSet> {
        self.read_with_limits(sql, limit, EXECUTION).await
    }

    async fn preview(&self, sql: &str, limit: usize) -> Result<ResultSet> {
        self.read_with_limits(
            sql,
            limit,
            connect::SessionLimits { statement_timeout: culprit_core::limits::PREVIEW_TIMEOUT, catalog_path: false },
        )
        .await
    }

    async fn execute(&self, sql: &str, limit: usize) -> Result<ResultSet> {
        let statement = single_statement(sql)?;
        let client = self.connector.connect(EXECUTION).await?;
        let started = Instant::now();
        // Sessions open read-only; a write the person chose to run gets a read-write session and commits on its own,
        // so statements that can't run in a transaction block (CREATE INDEX CONCURRENTLY, VACUUM) work too.
        client.batch_execute(&tagged("SET default_transaction_read_only = off")).await.map_err(map_error)?;
        let mut result = run_in_transaction(&client, statement, limit, false).await?;
        result.elapsed_ms = started.elapsed().as_secs_f64() * 1000.0;
        result.messages.push(t!("provider.postgres.result.committed"));
        Ok(result)
    }
}

/// Trim and reject empty input. Single-statement enforcement happens when the server parses it.
/// One statement, checked by Culprit's lexer under both standard_conforming_strings settings.
fn single_statement(sql: &str) -> Result<&str> {
    culprit_core::sql::single_statement(sql, Engine::Postgres)
}

/// Parse the statement on the server without running it. This also rejects several statements in one string. Tagged
/// like everything Culprit sends: pg_stat_statements keeps the text it first sees for a statement, and parsing one with
/// constants is enough to record it.
async fn parameter_count(client: &Client, statement: &str) -> Result<usize> {
    match client.prepare(&tagged(statement)).await {
        Ok(prepared) => Ok(prepared.params().len()),
        // Parameters whose type cannot be inferred: still a single statement, with placeholders.
        Err(e) if sql_state(&e) == Some("42P18") => Ok(1),
        Err(e) => Err(single_statement_error(e)),
    }
}

fn single_statement_error(error: tokio_postgres::Error) -> Error {
    if sql_state(&error) == Some("42601")
        && error.as_db_error().is_some_and(|d| d.message().contains("multiple commands"))
    {
        return Error::invalid(t!("provider.postgres.error.one_statement"))
            .with_hint(t!("provider.postgres.error.one_statement_hint"));
    }
    map_error(error)
}

async fn server_version(client: &Client) -> Result<u32> {
    let row = query_one(client, "SELECT current_setting('server_version_num') AS v").await?;
    Ok(row.and_then(|r| r.number("v")).unwrap_or(0.0) as u32)
}

async fn explain_text(client: &Client, sql: &str, kind: PlanKind, statement: &str) -> Result<Plan> {
    let rows = query(client, sql).await?;
    let json = rows
        .first()
        .and_then(|r| r.text("QUERY PLAN"))
        .ok_or_else(|| Error::internal(t!("provider.postgres.error.no_plan")))?;
    plan_json::parse(&json, kind, Some(statement))
}

/// Run one statement and collect up to `limit` rows; `cursor` fetches reads through a cursor (needs a transaction block).
async fn run_in_transaction(client: &Client, statement: &str, limit: usize, cursor: bool) -> Result<ResultSet> {
    let prepared = client.prepare(&tagged(statement)).await.map_err(single_statement_error)?;
    if !prepared.params().is_empty() {
        return Err(Error::invalid(t!("provider.postgres.error.parameters"))
            .with_hint(t!("provider.postgres.error.parameters_hint")));
    }
    let columns: Vec<String> = prepared.columns().iter().map(|c| c.name().to_string()).collect();
    let types: Vec<Type> = prepared.columns().iter().map(|c| c.type_().clone()).collect();
    let cursor_friendly = cursor
        && !columns.is_empty()
        && leading_keyword(statement).is_some_and(|k| matches!(k.as_str(), "select" | "with" | "values" | "table"));

    let query = if cursor_friendly {
        client
            .batch_execute(&tagged(&format!("DECLARE {CURSOR} NO SCROLL CURSOR FOR {statement}")))
            .await
            .map_err(map_error)?;
        format!("FETCH FORWARD {} FROM {CURSOR}", limit.min(results::MAX_ROWS).saturating_add(1))
    } else {
        statement.to_string()
    };
    results::collect(client, &tagged(&query), columns, &types, limit).await
}

fn strip_concurrently_keyword(stmt: &str) -> String {
    let mut cleaned = String::with_capacity(stmt.len());
    let mut chars = stmt.chars().peekable();
    let mut in_single = false;
    let mut in_double = false;
    while let Some(c) = chars.next() {
        if !in_single && !in_double {
            if c == '-' && chars.peek() == Some(&'-') {
                for ch in chars.by_ref() {
                    if ch == '\n' {
                        cleaned.push('\n');
                        break;
                    }
                }
                continue;
            }
            if c == '/' && chars.peek() == Some(&'*') {
                chars.next();
                while let Some(ch) = chars.next() {
                    if ch == '*' && chars.peek() == Some(&'/') {
                        chars.next();
                        break;
                    }
                }
                cleaned.push(' ');
                continue;
            }
        }
        if c == '\'' && !in_double {
            in_single = !in_single;
        } else if c == '"' && !in_single {
            in_double = !in_double;
        }
        cleaned.push(c);
    }
    let words: Vec<&str> = cleaned.split_whitespace().collect();
    if let Some(on_pos) = words.iter().position(|w| w.eq_ignore_ascii_case("ON")) {
        let mut head = Vec::with_capacity(on_pos);
        let mut i = 0;
        while i < on_pos {
            if words[i].eq_ignore_ascii_case("CONCURRENTLY") {
                i += 1;
                continue;
            }
            if words[i].eq_ignore_ascii_case("IF")
                && words.get(i + 1).is_some_and(|w| w.eq_ignore_ascii_case("NOT"))
                && words.get(i + 2).is_some_and(|w| w.eq_ignore_ascii_case("EXISTS"))
            {
                i += 3;
                continue;
            }
            head.push(words[i]);
            i += 1;
        }
        let on_byte = words[on_pos].as_ptr() as usize - cleaned.as_ptr() as usize;
        return format!("{} {}", head.join(" "), &cleaned[on_byte..]);
    }
    words.into_iter().filter(|w| !w.eq_ignore_ascii_case("CONCURRENTLY")).collect::<Vec<_>>().join(" ")
}

fn json_value(text: Option<String>, kind: Option<&Type>) -> Value {
    let Some(text) = text else { return Value::Null };
    match kind {
        Some(t) if *t == Type::BOOL => Value::Bool(text == "t"),
        Some(t) if [Type::INT2, Type::INT4, Type::INT8, Type::OID].contains(t) => {
            text.parse::<i64>().map(Value::from).unwrap_or(Value::String(text))
        }
        Some(t) if [Type::FLOAT4, Type::FLOAT8].contains(t) => text
            .parse::<f64>()
            .ok()
            .and_then(serde_json::Number::from_f64)
            .map(Value::Number)
            .unwrap_or(Value::String(text)),
        _ => Value::String(text),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use culprit_core::ErrorCode;

    #[test]
    fn values_follow_column_types() {
        assert_eq!(json_value(Some("t".into()), Some(&Type::BOOL)), Value::Bool(true));
        assert_eq!(json_value(Some("42".into()), Some(&Type::INT8)), Value::from(42));
        assert_eq!(json_value(Some("1.5".into()), Some(&Type::FLOAT8)), Value::from(1.5));
        assert_eq!(json_value(Some("1.50".into()), Some(&Type::NUMERIC)), Value::String("1.50".into()));
        assert_eq!(json_value(None, Some(&Type::TEXT)), Value::Null);
    }

    #[test]
    fn statements_are_trimmed() {
        assert_eq!(single_statement("  select 1 ;  ").unwrap(), "select 1");
        assert!(single_statement(" ; ").is_err());
        assert_eq!(
            strip_concurrently_keyword("CREATE INDEX CONCURRENTLY idx_orders_user ON public.orders (user_id)"),
            "CREATE INDEX idx_orders_user ON public.orders (user_id)"
        );
        assert_eq!(
            strip_concurrently_keyword(
                "CREATE UNIQUE INDEX CONCURRENTLY IF NOT EXISTS idx_orders_open ON public.orders (user_id) WHERE status = 'in  progress'"
            ),
            "CREATE UNIQUE INDEX idx_orders_open ON public.orders (user_id) WHERE status = 'in  progress'"
        );
    }

    #[test]
    fn rejects_other_engines() {
        let spec = ConnectionSpec { engine: Some(Engine::Mssql), host: "h".into(), ..Default::default() };
        let error = PostgresProvider::new(&spec).err().unwrap();
        assert_eq!(error.code, ErrorCode::InvalidInput);
        assert_eq!(error.message, "This connection is for SQL Server, not PostgreSQL");
    }
}
