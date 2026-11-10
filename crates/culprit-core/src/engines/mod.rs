//! Everything Culprit knows about each engine, one file per engine: its name, what it can answer, how it spells the SQL
//! Culprit suggests ([`Dialect`]), and how it says what every check needs to say about it (applying a setting,
//! refreshing statistics, what its statement statistics don't record). Checks ask the engine's [`EngineSpec`] instead of
//! matching the engine. A new engine is one more file here and one arm in [`Engine::spec`]; advice a single check gives
//! for one engine (how to fix a spill on PostgreSQL) stays with that check.
//!
//! Its words are messages in the language catalogs (`core.engines.*`): what Rust asks for is said in the current
//! language, and what the app reads (`app/src/gen/engines.json`, [`EngineFacts`]) are message keys, with ids its messages
//! `select` on where another language's words agree with the engine's (its log, its planning step).

mod mssql;
mod mysql;
mod postgres;
mod products;
mod sqlite;
#[cfg(test)]
mod tests;
mod timescale;
mod yugabyte;

pub use products::{DatabaseProduct, ProductFacts, ProductSpec};
pub use yugabyte::session_target as ysql_session_target;

use std::borrow::Cow;
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::dialect::Dialect;
use crate::error::{Error, ErrorCode};
use crate::finding::Fix;
use crate::i18n::{self, Arg};
use crate::model::{Ability, Engine, EngineAbilities, ServerInfo, SessionAction, wire_name};
use crate::{key, t};

/// How to apply a server setting: the SQL that does it (`sp_configure`, `ALTER SYSTEM` and a reload, `SET PERSIST`,
/// `PRAGMA`) and a sentence on where and how. A managed service changes settings in its own control instead; the checks'
/// `setting_change` says so before asking the engine.
#[derive(Clone, Debug, PartialEq)]
pub struct SettingChange {
    /// One sentence: where and how to set it.
    pub how: String,
    /// The statements, on servers where they work.
    pub sql: Option<String>,
}

/// One engine, as checks and every surface see it.
pub trait EngineSpec: Send + Sync {
    /// TLS policy and native option names; file engines have no network security settings.
    fn connection_security(&self) -> Option<crate::ConnectionSecuritySpec> {
        None
    }

    /// The provider's default product followed by any independently identified products it also serves.
    fn products(&self) -> &'static [ProductSpec];

    fn product(&self, recorded: &str) -> Option<&'static ProductSpec> {
        let products = self.products();
        products
            .iter()
            .find(|p| p.prefix.is_some_and(|prefix| recorded.starts_with(prefix)))
            .or_else(|| products.first())
    }
    fn sql_syntax(&self) -> SqlSyntax;
    /// Meaning and presentation of statement counters, independently of SQL grammar or storage.
    fn statement_metrics(&self) -> StatementMetrics;
    /// How a live wait sample is collected. Required so a new engine cannot inherit another engine's description.
    fn wait_sampling(&self) -> Option<WaitSampling>;
    /// Whether a health counter measures this database or a shared server, independently of its temporal scope.
    fn signal_scope(&self, server: &ServerInfo, key: &str) -> crate::MeasurementScope;
    /// A native label that already names its shared scope.
    fn scoped_signal_label(&self, _signal: &crate::Signal) -> Option<crate::i18n::Message> {
        None
    }
    /// Advice for the driver's session settings or reset command, after generic transactions and pings are handled.
    fn session_advice(&self, _command: &str) -> Option<String> {
        None
    }
    fn mutating_function(&self, _name: &str) -> bool {
        false
    }
    fn inactive_index_state(&self, server: &ServerInfo) -> crate::IndexState;
    /// A reversible alternative to dropping an index, only when this server supports it.
    fn hide_index(&self, _server: &ServerInfo, _schema: &str, _table: &str, _index: &str) -> Option<String> {
        None
    }
    fn unused_index_advice(&self, server: &ServerInfo, table_kind: Option<&str>) -> String;
    /// What a scan of it can't establish, as message keys: the app says them from its facts.
    fn limitation_keys(&self) -> &'static [&'static str];
    /// What a scan of it can't establish, in the current language.
    fn limitations(&self) -> Vec<String> {
        self.limitation_keys().iter().map(|key| i18n::t(key, &[])).collect()
    }
    /// Public collection limits, safe to show after private collector details have been removed for sharing.
    fn deadlock_limitation(&self) -> Option<String> {
        None
    }
    /// "SQL Server": the engine's name in sentences (a server's product can say more: MariaDB).
    fn label(&self) -> &'static str {
        self.products().first().map_or("Database", |p| p.label)
    }

    /// What it can answer: sessions, wait samples, measured runs… (see [`EngineAbilities`]).
    fn abilities(&self) -> EngineAbilities;

    /// Message key: what estimated plans are called here, by the command that asks for one ("Estimated plans
    /// (EXPLAIN)"): the label of [`crate::model::Capability::Showplan`] on this engine.
    fn estimated_plans_label(&self) -> &'static str;

    /// How it spells the SQL Culprit suggests.
    fn dialect(&self) -> &'static dyn Dialect;

    /// A storage-managed table may constrain DDL even when it speaks the server's usual SQL dialect.
    fn table_dialect(&self, _kind: Option<&str>) -> &'static dyn Dialect {
        self.dialect()
    }

    /// How to set `name` to `value` on a server Culprit reaches with SQL.
    fn change_setting(&self, server: &ServerInfo, name: &str, value: &str) -> SettingChange;

    /// How to set `name` to `value` for the scanned database alone, where the engine keeps settings per database and
    /// its owner may set this one without being a superuser (PostgreSQL's `ALTER DATABASE … SET` for a setting of
    /// context `user`). It works on a managed service too, where the server's own settings are the platform's. `None`
    /// where the setting is only the server's.
    fn database_setting(&self, _server: &ServerInfo, _name: &str, _value: &str) -> Option<SettingChange> {
        None
    }

    /// The fix for a table whose optimizer statistics are stale: recompute them (`target` already qualified), and how
    /// to keep them fresh.
    fn refresh_statistics(&self, target: &str) -> Fix;

    /// Why a wait in `category` ("io", "lock", "cpu"…) isn't put on any statement.
    fn wait_not_per_statement(&self, server: &ServerInfo, category: &str) -> String;

    /// Where the index advisor's candidates come from here, when that needs saying (no missing-index requests, no
    /// statement history). `has_workload`: the scan has statement statistics.
    fn index_advice_sources(&self, server: &ServerInfo, has_workload: bool) -> Option<String>;

    /// How a person connects to it, as the new-connection form asks.
    fn connect(&self) -> Connect;

    /// What may be done to another session from Live, outside production.
    fn session_actions(&self) -> Vec<SessionAction> {
        Vec::new()
    }

    /// Validate a Live session's engine-specific identity before resolving credentials or issuing an operation.
    fn validate_session_id(&self, session: &str) -> crate::Result<()> {
        crate::session_number(session).map(|_| ())
    }

    /// The word the engine uses for a statement that is waiting ("suspended" on SQL Server).
    fn waiting_state(&self) -> &'static str {
        "active"
    }

    /// Why it can't do `ability`, said the same by the desktop, the CLI, the MCP server and the browser demo. An engine
    /// overrides the ones it can say better than this.
    fn refusal(&self, _ability: Ability) -> Refusal {
        Refusal::cant()
    }
}

/// The SQL grammar a server speaks, independently of its storage engine and measurements.
#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
#[ts(export)]
pub enum SqlSyntax {
    Mssql,
    Postgres,
    Mysql,
    Sqlite,
}

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum WaitSampling {
    Sessions,
    Counters,
}

/// What the engine calls finding a statement's plan before running it: SQL Server compiles, the others plan. Messages
/// `select` on it (`{planning, select, compiling {..} other {..}}`), so another language's words can agree with it.
#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum PlanningStep {
    Planning,
    Compiling,
}

impl PlanningStep {
    /// The message key of its label: "Planning", "Compiling".
    pub fn label_key(self) -> &'static str {
        match self {
            PlanningStep::Planning => key!("core.engines.planning.planning"),
            PlanningStep::Compiling => key!("core.engines.planning.compiling"),
        }
    }
}

/// The log a statement's changes are written to before they reach the data files: PostgreSQL's WAL, the others'
/// transaction log. Messages `select` on it (`{log, select, wal {..} other {..}}`).
#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum WriteLog {
    Wal,
    TransactionLog,
}

impl WriteLog {
    /// The message key of its name in a sentence: "WAL", "log".
    pub fn name_key(self) -> &'static str {
        match self {
            WriteLog::Wal => key!("core.engines.log.wal"),
            WriteLog::TransactionLog => key!("core.engines.log.transaction_log"),
        }
    }

    /// The message key of the label of what a statement wrote to it: "WAL written", "Log written".
    pub fn written_key(self) -> &'static str {
        match self {
            WriteLog::Wal => key!("core.engines.log_written.wal"),
            WriteLog::TransactionLog => key!("core.engines.log_written.transaction_log"),
        }
    }
}

/// What a statement's reads are counted in: pages on most engines, rows examined on MySQL and MariaDB (the rows the
/// storage engine handed to the server). Messages `select` on it.
#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum ReadUnit {
    Pages,
    RowsExamined,
}

impl ReadUnit {
    /// The message key of a run's reads as a metric's label: "Pages read per run", "Rows examined per run".
    pub fn per_run_key(self) -> &'static str {
        match self {
            ReadUnit::Pages => key!("common.metric.pages_read_per_run"),
            ReadUnit::RowsExamined => key!("common.metric.rows_examined_per_run"),
        }
    }
}

/// What the row count of a statement that writes means: the rows it touched, the rows it changed, or (where the engine
/// counts nothing else) the rows it returned.
#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum WriteRows {
    Rows,
    Changed,
    Returned,
}

impl WriteRows {
    /// The message key of a run's rows as a metric's label: "Rows changed per run".
    pub fn per_run_key(self) -> &'static str {
        match self {
            WriteRows::Rows => key!("core.engines.write_rows.rows"),
            WriteRows::Changed => key!("core.engines.write_rows.changed"),
            WriteRows::Returned => key!("common.metric.rows_returned_per_run"),
        }
    }

    /// The message key of the rows of all its runs as a metric's label: "Rows changed".
    pub fn total_key(self) -> &'static str {
        match self {
            WriteRows::Rows => key!("common.metric.rows"),
            WriteRows::Changed => key!("core.engines.write_rows.changed_total"),
            WriteRows::Returned => key!("common.metric.rows_returned"),
        }
    }
}

/// Counter semantics shared with the app. Optional facts describe support; a particular scan may still lack data. Its
/// words are message keys, said with [`i18n::t`]; a note's `{engine}` is the server's display name, including product
/// variants such as MariaDB.
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct StatementMetrics {
    pub planning: PlanningStep,
    /// Message key: what [`StatementMetrics::planning`] is called ("Compiling").
    pub planning_label: Cow<'static, str>,
    pub planning_setting: Option<Cow<'static, str>>,
    pub compilation_history: bool,
    pub reads: ReadUnit,
    /// Message key: a run's reads ("Pages read per run").
    pub reads_label: Cow<'static, str>,
    /// What a write's row count means; `None` where the engine counts no rows for a write (MySQL).
    pub write_rows: Option<WriteRows>,
    /// A run's rows of a write, said in the current language when the facts were asked for ("Rows changed per run"),
    /// for the checks that put it on a metric. The app has no use for it.
    #[serde(skip)]
    #[ts(skip)]
    pub write_rows_label: Option<Cow<'static, str>>,
    pub lock_time: bool,
    pub log: WriteLog,
    /// Message key: [`StatementMetrics::log`] in a sentence ("WAL").
    pub log_name: Cow<'static, str>,
    /// Message key: the label of what a statement wrote to it ("WAL written").
    pub log_label: Cow<'static, str>,
    pub missing_costs_note: Option<Cow<'static, str>>,
    pub errors_note: Option<Cow<'static, str>>,
    pub runtime_note: Cow<'static, str>,
    pub runtime_histogram_note: Option<Cow<'static, str>>,
    pub runtime_cache_note: Option<Cow<'static, str>>,
}

impl StatementMetrics {
    /// The words for an engine's planning step, reads, write rows and log, with the run-time note of an engine whose
    /// statistics are aggregates; its file says what else it keeps.
    fn new(planning: PlanningStep, reads: ReadUnit, write_rows: Option<WriteRows>, log: WriteLog) -> Self {
        Self {
            planning,
            planning_label: planning.label_key().into(),
            planning_setting: None,
            compilation_history: false,
            reads,
            reads_label: reads.per_run_key().into(),
            write_rows,
            write_rows_label: write_rows.map(|rows| i18n::t(rows.per_run_key(), &[]).into()),
            lock_time: false,
            log,
            log_name: log.name_key().into(),
            log_label: log.written_key().into(),
            missing_costs_note: None,
            errors_note: None,
            runtime_note: key!("core.engines.runtime_note").into(),
            runtime_histogram_note: None,
            runtime_cache_note: None,
        }
    }

    /// YSQL and PostgreSQL share these statement statistics, without sharing heap or WAL measurements.
    fn pg_stat_statements(log: WriteLog) -> Self {
        Self {
            planning_setting: Some("pg_stat_statements.track_planning".into()),
            missing_costs_note: Some(key!("core.engines.pg_stat_statements.missing_costs_note").into()),
            runtime_note: key!("core.engines.pg_stat_statements.runtime_note").into(),
            ..Self::new(PlanningStep::Planning, ReadUnit::Pages, Some(WriteRows::Changed), log)
        }
    }
}

/// Why an engine can't do something: the message and, when there's one, what to do instead, as message keys. Each is
/// said with the engine's label as `{engine}` and the ability's id as `{ability}` ([`Engine::refused`]).
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct Refusal {
    pub message: String,
    pub hint: Option<String>,
}

impl Refusal {
    fn new(message: &'static str, hint: Option<&'static str>) -> Refusal {
        Refusal { message: message.into(), hint: hint.map(str::to_string) }
    }

    /// "PostgreSQL can't do this (last actual plan)": for an ability an engine has nothing better to say about.
    fn cant() -> Refusal {
        Refusal::new(key!("core.engines.refusal.cant"), None)
    }
}

/// How a person connects to an engine: the file they choose, or what they paste and how Culprit reads it.
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct Connect {
    /// The engine where a person picks it, with what else it serves: "MySQL / MariaDB".
    pub choice: String,
    /// The database is a file on this computer (SQLite), chosen rather than pasted.
    pub file: bool,
    /// What to paste, or the path to type, as an example.
    pub example: String,
    /// Message key: what Culprit reads, in a sentence.
    pub accepts: String,
}

impl Connect {
    fn server(choice: &str, example: &str, accepts: &'static str) -> Connect {
        Connect { choice: choice.into(), file: false, example: example.into(), accepts: accepts.into() }
    }
}

/// Everything the app needs to know about an engine without a connection: written to `app/src/gen/engines.json`.
#[derive(Serialize, Deserialize, TS, Clone, Debug)]
#[ts(export)]
pub struct EngineFacts {
    pub label: String,
    pub products: Vec<ProductFacts>,
    pub abilities: EngineAbilities,
    pub session_actions: Vec<SessionAction>,
    pub waiting_state: String,
    pub connect: Connect,
    /// Message keys: what a scan of it can't establish.
    pub limitations: Vec<String>,
    /// The reason for every ability it lacks, by ability.
    pub refusals: BTreeMap<String, Refusal>,
    pub sql_syntax: SqlSyntax,
    pub statement_metrics: StatementMetrics,
    pub wait_sampling: Option<WaitSampling>,
    pub connection_security: Option<crate::ConnectionSecuritySpec>,
}

impl Engine {
    /// Everything Culprit knows about this engine.
    pub fn spec(self) -> &'static dyn EngineSpec {
        match self {
            Engine::Mssql => &mssql::Mssql,
            Engine::Postgres => &postgres::Postgres,
            Engine::Yugabyte => &yugabyte::Yugabyte,
            Engine::Mysql => &mysql::Mysql,
            Engine::Sqlite => &sqlite::Sqlite,
        }
    }

    pub fn label(self) -> &'static str {
        self.spec().label()
    }

    pub fn sql_syntax(self) -> SqlSyntax {
        self.spec().sql_syntax()
    }

    /// What the engine can answer, defined in its own file: the CLI, the MCP server and the app offer only these, and
    /// the app reads this same table (`app/src/gen/abilities.json`, written by this crate's tests).
    pub fn abilities(self) -> EngineAbilities {
        self.spec().abilities()
    }

    /// How this engine spells the SQL Culprit suggests.
    pub fn dialect(self) -> &'static dyn Dialect {
        self.spec().dialect()
    }

    /// The error a surface returns when asked for something the engine can't do, in the current language.
    pub fn refused(self, ability: Ability) -> Error {
        let r = self.spec().refusal(ability);
        let id = wire_name(&ability);
        let args = [("engine", Arg::from(self.label())), ("ability", Arg::from(id.as_str()))];
        let error = Error::new(ErrorCode::Unsupported, i18n::t(&r.message, &args));
        match r.hint {
            Some(hint) => error.with_hint(i18n::t(&hint, &args)),
            None => error,
        }
    }

    /// What the app reads about this engine (see [`EngineFacts`]).
    pub fn facts(self) -> EngineFacts {
        let spec = self.spec();
        let abilities = spec.abilities();
        EngineFacts {
            connection_security: spec.connection_security(),
            products: spec.products().iter().map(|product| product.facts()).collect(),
            statement_metrics: spec.statement_metrics(),
            wait_sampling: spec.wait_sampling(),
            sql_syntax: spec.sql_syntax(),
            label: spec.label().into(),
            abilities,
            session_actions: spec.session_actions(),
            waiting_state: spec.waiting_state().into(),
            connect: spec.connect(),
            limitations: spec.limitation_keys().iter().map(|key| key.to_string()).collect(),
            refusals: Ability::ALL
                .into_iter()
                .filter(|a| !abilities.has(*a))
                .map(|a| (wire_name(&a), spec.refusal(a)))
                .collect(),
        }
    }
}

/// A setting's value as a SQL literal: a number as it is, anything else quoted.
fn literal(value: &str) -> String {
    if value.chars().all(|c| c.is_ascii_digit() || c == '.') { value.to_string() } else { format!("'{value}'") }
}

/// A server engine Culprit talks to: everything but what only one engine keeps (SQL Server's last actual plans and live
/// per-step counters), which that engine adds.
const SERVER: EngineAbilities = EngineAbilities {
    sessions: true,
    wait_sample: true,
    deadlocks: true,
    measures_runs: true,
    plan_estimates: true,
    writes: true,
    periods: true,
    last_actual_plan: false,
    operator_progress: false,
    sign_in: true,
};
