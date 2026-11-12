//! What each engine says, word for word, in English: its facts as the app reads them (said from the catalog), and the
//! advice, settings, fixes, notes and refusals the checks and surfaces ask it for.

use super::*;

#[test]
fn constraint_and_io_semantics_are_explicit_for_every_engine() {
    use ConstraintValidation::*;
    let cases = [
        (Engine::Mssql, OptimizerTrust, false),
        (Engine::Postgres, CheckExclusion, true),
        (Engine::Yugabyte, Deferred, false),
        (Engine::Mysql, Unsupported, false),
        (Engine::Sqlite, Unsupported, false),
    ];
    assert_eq!(cases.map(|c| c.0), Engine::ALL, "Declare native semantics for each new engine");
    for (engine, constraints, io_is_component) in cases {
        let spec = engine.spec();
        assert_eq!(spec.constraint_validation(), constraints, "{engine:?}");
        assert_eq!(spec.statement_metrics().io_wait_within_statement_time, io_is_component, "{engine:?}");
        let facts = engine.facts();
        assert_eq!(facts.constraint_validation, constraints);
        assert_eq!(facts.statement_metrics.io_wait_within_statement_time, io_is_component);
    }
}

#[test]
fn partition_tail_semantics_are_explicit_for_every_engine_and_shared_with_frontend_facts() {
    for engine in Engine::ALL {
        let expected = match engine {
            Engine::Mssql => PartitionTail::Unbounded,
            Engine::Postgres | Engine::Mysql | Engine::Yugabyte => PartitionTail::FromCatalog,
            Engine::Sqlite => PartitionTail::Unsupported,
        };
        assert_eq!(engine.spec().partition_tail(), expected);
        assert_eq!(engine.facts().partition_tail, expected);
    }
}
use crate::i18n::{Arg, t};

fn server(engine: Engine, product: &str, version: &str, capabilities: Vec<Capability>) -> ServerInfo {
    ServerInfo {
        engine,
        product: product.into(),
        version: version.into(),
        edition: None,
        cloud: None,
        platform: None,
        database: "shop".into(),
        database_size_bytes: None,
        started_at: None,
        read_only_session: true,
        capabilities,
        missing: vec![],
    }
}

/// A fact the app says: a message key, with the arguments the app gives it.
fn say(text: &str, args: &[(&str, Arg)]) -> String {
    t(text, args)
}

use crate::model::Capability;

/// Every word an engine says, one line each.
fn words(engine: Engine) -> Vec<String> {
    let spec = engine.spec();
    let facts = engine.facts();
    let label = spec.label();
    let mut out = Vec::new();
    let mut line = |what: &str, text: String| out.push(format!("{engine:?}.{what}: {text}"));
    for p in &facts.products {
        line(&format!("plan_format.{:?}", p.id), say(&p.plan_format, &[]));
    }
    line("accepts", say(&facts.connect.accepts, &[]));
    line("waiting_state", facts.waiting_state.clone());
    for (i, l) in facts.limitations.iter().enumerate() {
        line(&format!("limitation.{i}"), say(l, &[]));
    }
    for (i, l) in spec.limitations().iter().enumerate() {
        line(&format!("limitations.{i}"), l.to_string());
    }
    for (ability, r) in &facts.refusals {
        let args = [("engine", Arg::from(label)), ("ability", Arg::from(ability.as_str()))];
        line(&format!("refusal.{ability}"), say(&r.message, &args));
        if let Some(hint) = &r.hint {
            line(&format!("refusal.{ability}.hint"), say(hint, &args));
        }
    }
    for ability in crate::model::Ability::ALL {
        let e = engine.refused(ability);
        line(&format!("refused.{}", wire_name(&ability)), e.message);
        if let Some(hint) = e.hint {
            line(&format!("refused.{}.hint", wire_name(&ability)), hint);
        }
    }
    let m = spec.statement_metrics();
    let names: Vec<&str> = spec.products().iter().map(|p| p.label).collect();
    line("metrics.planning_label", say(&m.planning_label, &[]));
    line("metrics.write_rows_label", format!("{:?}", m.write_rows_label.as_deref()));
    line("metrics.log_name", say(&m.log_name, &[]));
    line("metrics.log_label", say(&m.log_label, &[]));
    for name in &names {
        let note = |n: &Option<Cow<'static, str>>| {
            n.as_deref().map(|n| say(n, &[("engine", Arg::from(*name))]).replace("{engine}", name))
        };
        line(&format!("metrics.missing_costs_note.{name}"), format!("{:?}", note(&m.missing_costs_note)));
        line(&format!("metrics.errors_note.{name}"), format!("{:?}", note(&m.errors_note)));
        line(&format!("metrics.runtime_note.{name}"), format!("{:?}", note(&Some(m.runtime_note.clone()))));
        line(&format!("metrics.runtime_histogram_note.{name}"), format!("{:?}", note(&m.runtime_histogram_note)));
        line(&format!("metrics.runtime_cache_note.{name}"), format!("{:?}", note(&m.runtime_cache_note)));
    }
    for command in ["discard", "reset", "set"] {
        line(
            &format!("session_advice.{command}"),
            format!("{:?}", spec.session_advice(command).map(|s| s.to_string())),
        );
    }
    line("deadlock_limitation", format!("{:?}", spec.deadlock_limitation().map(|s| s.to_string())));
    let servers: Vec<ServerInfo> = match engine {
        Engine::Mysql => vec![
            server(engine, "MySQL 8.4", "8.4.2", vec![Capability::PerformanceSchema]),
            server(engine, "MySQL 5.7", "5.7.44-log", vec![]),
            server(engine, "MariaDB 11.8.9", "11.8.9-MariaDB", vec![Capability::PerformanceSchema]),
            server(engine, "MariaDB 10.5", "10.5.29-MariaDB-ubu2004", vec![]),
            server(engine, "MySQL", "", vec![]),
        ],
        Engine::Mssql => vec![
            server(engine, "SQL Server 2022", "16.0", vec![Capability::MissingIndexes]),
            server(engine, "SQL Server 2022", "16.0", vec![]),
        ],
        _ => vec![server(engine, label, "16.4", vec![])],
    };
    for (i, s) in servers.iter().enumerate() {
        for kind in [None, Some("hypertable"), Some("timescale-chunk")] {
            line(&format!("unused_index_advice.{i}.{kind:?}"), spec.unused_index_advice(s, kind).to_string());
        }
        for (name, value) in [("work_mem", "64MB"), ("max_connections", "200")] {
            let c = spec.change_setting(s, name, value);
            line(&format!("change_setting.{i}.{name}"), format!("{} | {:?}", c.how, c.sql));
            let d = spec.database_setting(s, name, value);
            line(&format!("database_setting.{i}.{name}"), format!("{:?}", d.map(|d| (d.how, d.sql))));
        }
        for category in ["io", "lock", "cpu", "memory"] {
            line(&format!("wait_not_per_statement.{i}.{category}"), spec.wait_not_per_statement(s, category));
        }
        for workload in [true, false] {
            line(
                &format!("index_advice_sources.{i}.{workload}"),
                format!("{:?}", spec.index_advice_sources(s, workload)),
            );
        }
    }
    let fix = spec.refresh_statistics("public.orders");
    line("refresh_statistics", format!("{} | {:?} | {:?}", fix.summary, fix.sql, fix.cost));
    out
}

#[test]
fn every_engine_says_its_facts_and_advice_in_english() {
    let mut all: Vec<String> = Engine::ALL.into_iter().flat_map(words).collect();
    all.push(format!("Yugabyte.session_target: {}", crate::engines::ysql_session_target("42").unwrap_err().message));
    let actual = all.join("\n");
    assert_eq!(actual, EXPECTED);
}

// The words checks and the app choose by engine are its facts: every engine declares its planning step, its log, what
// its reads count and what a write's rows mean, each said by a message English has, with the id messages select on.
#[test]
fn every_engine_declares_its_vocabulary_as_messages_and_ids() {
    use PlanningStep::{Compiling, Planning};
    use ReadUnit::{Pages, RowsExamined};
    use WriteLog::{TransactionLog, Wal};
    use WriteRows::{Changed, Returned, Rows};
    let cases = [
        (Engine::Mssql, Compiling, TransactionLog, Pages, Some(Rows)),
        (Engine::Postgres, Planning, Wal, Pages, Some(Changed)),
        (Engine::Yugabyte, Planning, TransactionLog, Pages, Some(Changed)),
        (Engine::Mysql, Planning, TransactionLog, RowsExamined, None),
        (Engine::Sqlite, Planning, TransactionLog, Pages, Some(Returned)),
    ];
    assert_eq!(cases.map(|c| c.0), Engine::ALL, "Declare the vocabulary of each new engine explicitly");
    let english = crate::i18n::english();
    for (engine, planning, log, reads, rows) in cases {
        let m = engine.spec().statement_metrics();
        assert_eq!((m.planning, m.log, m.reads, m.write_rows), (planning, log, reads, rows), "{engine:?}");
        assert_eq!(m.write_rows_label.as_deref(), rows.map(|r| t(r.per_run_key(), &[])).as_deref());
        // Every word the app reads from its facts is a message English has.
        let facts = engine.facts();
        let notes = [&m.missing_costs_note, &m.errors_note, &m.runtime_histogram_note, &m.runtime_cache_note];
        let keys = [&m.planning_label, &m.reads_label, &m.log_name, &m.log_label, &m.runtime_note]
            .into_iter()
            .map(|k| k.to_string())
            .chain(notes.into_iter().flatten().map(|k| k.to_string()))
            .chain(facts.products.iter().map(|p| p.plan_format.clone()))
            .chain([facts.connect.accepts.clone(), engine.spec().estimated_plans_label().to_string()])
            .chain(facts.limitations.iter().cloned())
            .chain(facts.refusals.values().flat_map(|r| [Some(r.message.clone()), r.hint.clone()]).flatten());
        for key in keys {
            assert!(english.message(&key).is_some(), "{engine:?}: {key} is not in locales/en.json");
        }
        // The app's messages select on the ids, and say what the engine's own words say.
        let share = || ("share", Arg::from("40%"));
        assert_eq!(
            t("ui.beyond_time.of_all_log", &[share(), ("log", Arg::from(wire_name(&log)))]),
            format!("40% of all {}", t(&m.log_name, &[]))
        );
        assert_eq!(
            t("ui.beyond_time.takeaway.planning", &[share(), ("planning", Arg::from(wire_name(&planning)))]),
            format!("{} takes 40% of its time.", t(&m.planning_label, &[]))
        );
    }
    let words = |key: &str| t(key, &[]);
    assert_eq!(
        [Rows, Changed, Returned].map(|r| (words(r.per_run_key()), words(r.total_key()))),
        [
            ("Rows per run".into(), "Rows".into()),
            ("Rows changed per run".into(), "Rows changed".into()),
            ("Rows returned per run".into(), "Rows returned".into()),
        ]
    );
    assert_eq!([Pages, RowsExamined].map(|r| words(r.per_run_key())), ["Pages read per run", "Rows examined per run"]);
    assert_eq!([Wal, TransactionLog].map(|l| words(l.name_key())), ["WAL", "log"]);
    assert_eq!([Wal, TransactionLog].map(|l| words(l.written_key())), ["WAL written", "Log written"]);
    assert_eq!([Planning, Compiling].map(|p| words(p.label_key())), ["Planning", "Compiling"]);
}

const EXPECTED: &str = r#"Mssql.plan_format.Mssql: .sqlplan or showplan XML (SSMS, Query Store)
Mssql.accepts: An ADO.NET connection string, an mssql:// or JDBC URL, or host,port/database. Azure SQL without a user name signs in with your az login (Entra ID).
Mssql.waiting_state: suspended
Mssql.limitation.0: Query Store records aggregate executions and plan history, not an application's request trace or every parameter value. Caller identity and transaction boundaries need application telemetry.
Mssql.limitation.1: An estimated or stored compilation plan does not measure the rows, timing or waits of the run you are investigating. Runtime-only problems require recorded runtime evidence or an explicitly measured run.
Mssql.limitation.2: Host CPU and filesystem capacity need their own measurements, such as connected cloud telemetry. Backup recoverability and database corruption are not checked by the statistics scan; they need backup/recovery validation and an appropriate integrity investigation.
Mssql.limitations.0: Query Store records aggregate executions and plan history, not an application's request trace or every parameter value. Caller identity and transaction boundaries need application telemetry.
Mssql.limitations.1: An estimated or stored compilation plan does not measure the rows, timing or waits of the run you are investigating. Runtime-only problems require recorded runtime evidence or an explicitly measured run.
Mssql.limitations.2: Host CPU and filesystem capacity need their own measurements, such as connected cloud telemetry. Backup recoverability and database corruption are not checked by the statistics scan; they need backup/recovery validation and an appropriate integrity investigation.
Mssql.refused.sessions: SQL Server can't do this (sessions)
Mssql.refused.wait_sample: SQL Server can't do this (wait sample)
Mssql.refused.deadlocks: SQL Server can't do this (deadlocks)
Mssql.refused.measures_runs: SQL Server can't do this (measures runs)
Mssql.refused.plan_estimates: SQL Server can't do this (plan estimates)
Mssql.refused.writes: SQL Server can't do this (writes)
Mssql.refused.periods: SQL Server can't do this (periods)
Mssql.refused.last_actual_plan: SQL Server can't do this (last actual plan)
Mssql.refused.operator_progress: SQL Server can't do this (operator progress)
Mssql.refused.sign_in: SQL Server can't do this (sign in)
Mssql.metrics.planning_label: Compiling
Mssql.metrics.write_rows_label: Some("Rows per run")
Mssql.metrics.log_name: log
Mssql.metrics.log_label: Log written
Mssql.metrics.missing_costs_note.SQL Server: None
Mssql.metrics.errors_note.SQL Server: Some("Query Store counts errors but doesn't keep them; the finding shows how to capture them.")
Mssql.metrics.runtime_note.SQL Server: Some("Query Store keeps the fastest and slowest run and the spread of every plan, and each plan's compiled parameter values.")
Mssql.metrics.runtime_histogram_note.SQL Server: None
Mssql.metrics.runtime_cache_note.SQL Server: Some("The plan cache keeps the fastest and slowest run of each cached plan since it was cached, not their spread: Query Store would add it.")
Mssql.session_advice.discard: Some("The driver or the connection pool sends this around the application's statements, not the application's own code: a pooled connection returns to the login's defaults each time it is reused (sp_reset_connection), so options set per session are sent again on every checkout. Set them once instead: as the database's or the login's defaults, or in the connection string where the driver allows.")
Mssql.session_advice.reset: Some("The driver or the connection pool sends this around the application's statements, not the application's own code: a pooled connection returns to the login's defaults each time it is reused (sp_reset_connection), so options set per session are sent again on every checkout. Set them once instead: as the database's or the login's defaults, or in the connection string where the driver allows.")
Mssql.session_advice.set: Some("The driver or the connection pool sends this around the application's statements, not the application's own code: a pooled connection returns to the login's defaults each time it is reused (sp_reset_connection), so options set per session are sent again on every checkout. Set them once instead: as the database's or the login's defaults, or in the connection string where the driver allows.")
Mssql.deadlock_limitation: None
Mssql.unused_index_advice.0.None: Disable first: the definition stays, nothing reads it, and `ALTER INDEX … REBUILD` brings it back. Drop it after a full business cycle without complaints.
Mssql.unused_index_advice.0.Some("hypertable"): Disable first: the definition stays, nothing reads it, and `ALTER INDEX … REBUILD` brings it back. Drop it after a full business cycle without complaints.
Mssql.unused_index_advice.0.Some("timescale-chunk"): Disable first: the definition stays, nothing reads it, and `ALTER INDEX … REBUILD` brings it back. Drop it after a full business cycle without complaints.
Mssql.change_setting.0.work_mem: Set work_mem to 64MB. | Some("EXEC sp_configure 'work_mem', '64MB';\nRECONFIGURE;")
Mssql.database_setting.0.work_mem: None
Mssql.change_setting.0.max_connections: Set max_connections to 200. | Some("EXEC sp_configure 'max_connections', 200;\nRECONFIGURE;")
Mssql.database_setting.0.max_connections: None
Mssql.wait_not_per_statement.0.io: Query Store didn't record this wait for individual queries in the window.
Mssql.wait_not_per_statement.0.lock: Query Store didn't record this wait for individual queries in the window.
Mssql.wait_not_per_statement.0.cpu: Query Store didn't record this wait for individual queries in the window.
Mssql.wait_not_per_statement.0.memory: Query Store didn't record this wait for individual queries in the window.
Mssql.index_advice_sources.0.true: None
Mssql.index_advice_sources.0.false: None
Mssql.unused_index_advice.1.None: Disable first: the definition stays, nothing reads it, and `ALTER INDEX … REBUILD` brings it back. Drop it after a full business cycle without complaints.
Mssql.unused_index_advice.1.Some("hypertable"): Disable first: the definition stays, nothing reads it, and `ALTER INDEX … REBUILD` brings it back. Drop it after a full business cycle without complaints.
Mssql.unused_index_advice.1.Some("timescale-chunk"): Disable first: the definition stays, nothing reads it, and `ALTER INDEX … REBUILD` brings it back. Drop it after a full business cycle without complaints.
Mssql.change_setting.1.work_mem: Set work_mem to 64MB. | Some("EXEC sp_configure 'work_mem', '64MB';\nRECONFIGURE;")
Mssql.database_setting.1.work_mem: None
Mssql.change_setting.1.max_connections: Set max_connections to 200. | Some("EXEC sp_configure 'max_connections', 200;\nRECONFIGURE;")
Mssql.database_setting.1.max_connections: None
Mssql.wait_not_per_statement.1.io: Query Store didn't record this wait for individual queries in the window.
Mssql.wait_not_per_statement.1.lock: Query Store didn't record this wait for individual queries in the window.
Mssql.wait_not_per_statement.1.cpu: Query Store didn't record this wait for individual queries in the window.
Mssql.wait_not_per_statement.1.memory: Query Store didn't record this wait for individual queries in the window.
Mssql.index_advice_sources.1.true: Some("This login can't read SQL Server's missing-index requests (VIEW DATABASE STATE), so candidates come from captured plans and foreign keys only.")
Mssql.index_advice_sources.1.false: Some("This login can't read SQL Server's missing-index requests (VIEW DATABASE STATE), so candidates come from captured plans and foreign keys only.")
Mssql.refresh_statistics: Update the statistics now; if this table changes fast, schedule the update in a maintenance job. | Some("UPDATE STATISTICS public.orders;") | Some("Reads a sample of the table; run it outside peak hours on very large tables.")
Postgres.plan_format.Postgres: EXPLAIN or EXPLAIN ANALYZE, as text or FORMAT JSON
Postgres.plan_format.Timescale: PostgreSQL EXPLAIN, including hypertable and chunk operators
Postgres.accepts: A postgres:// or JDBC URL, libpq's key=value pairs, an Npgsql connection string, or host:port/database.
Postgres.waiting_state: active
Postgres.limitation.0: pg_stat_statements keeps aggregate totals, not per-request traces, historical parameter values or the plan used by each execution. Culprit's scans provide comparisons only over their recorded intervals.
Postgres.limitation.1: A newly obtained generic plan uses this connection's role, settings and search path; it may differ from the application's custom plan. Actual rows, spills and timing require a measured plan.
Postgres.limitation.2: Deadlock graphs, statement errors and auto_explain output live in server logs. SQL counters alone cannot reconstruct them; import the relevant logs or correlate application telemetry.
Postgres.limitations.0: pg_stat_statements keeps aggregate totals, not per-request traces, historical parameter values or the plan used by each execution. Culprit's scans provide comparisons only over their recorded intervals.
Postgres.limitations.1: A newly obtained generic plan uses this connection's role, settings and search path; it may differ from the application's custom plan. Actual rows, spills and timing require a measured plan.
Postgres.limitations.2: Deadlock graphs, statement errors and auto_explain output live in server logs. SQL counters alone cannot reconstruct them; import the relevant logs or correlate application telemetry.
Postgres.refusal.last_actual_plan: PostgreSQL doesn't keep the actual plan of past runs
Postgres.refusal.last_actual_plan.hint: auto_explain can write them to the server log, which Culprit can't read over SQL. Open the query in the editor with real values and use Analyze: it runs inside a transaction that is rolled back.
Postgres.refusal.operator_progress: PostgreSQL reports a running statement's progress, not each step's
Postgres.refusal.operator_progress.hint: Its plan doesn't change while it runs: ask for the plan once.
Postgres.refused.sessions: PostgreSQL can't do this (sessions)
Postgres.refused.wait_sample: PostgreSQL can't do this (wait sample)
Postgres.refused.deadlocks: PostgreSQL can't do this (deadlocks)
Postgres.refused.measures_runs: PostgreSQL can't do this (measures runs)
Postgres.refused.plan_estimates: PostgreSQL can't do this (plan estimates)
Postgres.refused.writes: PostgreSQL can't do this (writes)
Postgres.refused.periods: PostgreSQL can't do this (periods)
Postgres.refused.last_actual_plan: PostgreSQL doesn't keep the actual plan of past runs
Postgres.refused.last_actual_plan.hint: auto_explain can write them to the server log, which Culprit can't read over SQL. Open the query in the editor with real values and use Analyze: it runs inside a transaction that is rolled back.
Postgres.refused.operator_progress: PostgreSQL reports a running statement's progress, not each step's
Postgres.refused.operator_progress.hint: Its plan doesn't change while it runs: ask for the plan once.
Postgres.refused.sign_in: PostgreSQL can't do this (sign in)
Postgres.metrics.planning_label: Planning
Postgres.metrics.write_rows_label: Some("Rows changed per run")
Postgres.metrics.log_name: WAL
Postgres.metrics.log_label: WAL written
Postgres.metrics.missing_costs_note.PostgreSQL: Some("PostgreSQL keeps no error count or lock time per statement; Live shows blocking as it happens.")
Postgres.metrics.errors_note.PostgreSQL: None
Postgres.metrics.runtime_note.PostgreSQL: Some("PostgreSQL keeps one fastest, average, slowest and spread per statement: enough to see a long tail, not which values caused it.")
Postgres.metrics.runtime_histogram_note.PostgreSQL: None
Postgres.metrics.runtime_cache_note.PostgreSQL: None
Postgres.metrics.missing_costs_note.TimescaleDB: Some("TimescaleDB keeps no error count or lock time per statement; Live shows blocking as it happens.")
Postgres.metrics.errors_note.TimescaleDB: None
Postgres.metrics.runtime_note.TimescaleDB: Some("TimescaleDB keeps one fastest, average, slowest and spread per statement: enough to see a long tail, not which values caused it.")
Postgres.metrics.runtime_histogram_note.TimescaleDB: None
Postgres.metrics.runtime_cache_note.TimescaleDB: None
Postgres.session_advice.discard: Some("This resets session state and is commonly sent by a driver or connection pool; the statement statistics do not identify its caller or pooling mode. Inspect the application's connection lifecycle and the driver or pool configuration, and benchmark any change against the measured absolute cost shown here. Keep resets that prevent session state leaking between users. Npgsql arranges its reset on close and sends it on reuse; PgBouncer normally resets on release in session pooling, and normally does not in transaction pooling. Consider changing reset behavior only after verifying that every caller safely handles retained session state.")
Postgres.session_advice.reset: Some("This resets session state and is commonly sent by a driver or connection pool; the statement statistics do not identify its caller or pooling mode. Inspect the application's connection lifecycle and the driver or pool configuration, and benchmark any change against the measured absolute cost shown here. Keep resets that prevent session state leaking between users. Npgsql arranges its reset on close and sends it on reuse; PgBouncer normally resets on release in session pooling, and normally does not in transaction pooling. Consider changing reset behavior only after verifying that every caller safely handles retained session state.")
Postgres.session_advice.set: None
Postgres.deadlock_limitation: Some("PostgreSQL keeps a cumulative deadlock count in SQL, but the graphs and statements are only in the server log. Import the relevant log entries to explain the pattern. Turning on log_lock_waits also logs waits longer than deadlock_timeout.")
Postgres.unused_index_advice.0.None: Drop it concurrently so writes aren't blocked. Save the definition first so you can recreate it.
Postgres.unused_index_advice.0.Some("hypertable"): Save the definition first so you can recreate it. TimescaleDB does not support dropping hypertable indexes concurrently; schedule the drop for a maintenance window.
Postgres.unused_index_advice.0.Some("timescale-chunk"): Save the definition first so you can recreate it. TimescaleDB does not support dropping hypertable indexes concurrently; schedule the drop for a maintenance window.
Postgres.change_setting.0.work_mem: Set work_mem to 64MB and reload the configuration (settings marked as needing a restart take effect after one). | Some("ALTER SYSTEM SET work_mem = '64MB';\nSELECT pg_reload_conf();")
Postgres.database_setting.0.work_mem: Some(("Set work_mem to 64MB for this database only, as its owner: sessions that connect after the change use it, and other databases on the server keep theirs.", Some("ALTER DATABASE shop SET work_mem = '64MB';")))
Postgres.change_setting.0.max_connections: Set max_connections to 200 and reload the configuration (settings marked as needing a restart take effect after one). | Some("ALTER SYSTEM SET max_connections = 200;\nSELECT pg_reload_conf();")
Postgres.database_setting.0.max_connections: None
Postgres.wait_not_per_statement.0.io: No statement recorded disk-read time.
Postgres.wait_not_per_statement.0.lock: PostgreSQL doesn't time this per statement. Right now samples what sessions wait on.
Postgres.wait_not_per_statement.0.cpu: PostgreSQL doesn't time this per statement. Right now samples what sessions wait on.
Postgres.wait_not_per_statement.0.memory: PostgreSQL doesn't time this per statement. Right now samples what sessions wait on.
Postgres.index_advice_sources.0.true: Some("PostgreSQL doesn't record missing-index requests. Candidates come from captured plans that read whole tables, foreign keys, and the table scan counters.")
Postgres.index_advice_sources.0.false: Some("PostgreSQL doesn't record missing-index requests. Candidates come from captured plans that read whole tables, foreign keys, and the table scan counters.")
Postgres.refresh_statistics: Analyze the table now. If autovacuum falls behind on it regularly, lower its analyze threshold for this table only. | Some("ANALYZE public.orders;\nALTER TABLE public.orders SET (autovacuum_analyze_scale_factor = 0.02);") | Some("ANALYZE reads a sample of the table and does not block reads or writes.")
Yugabyte.plan_format.Yugabyte: EXPLAIN (ANALYZE, DIST, FORMAT JSON), including DocDB storage metrics
Yugabyte.accepts: A YSQL postgres:// or yugabyte:// URL, libpq parameters, or host:port/database.
Yugabyte.waiting_state: active
Yugabyte.limitation.0: YSQL uses DocDB/RocksDB over Raft. PostgreSQL heap pages, buffer-cache ratios, dead tuples, vacuum and XID ages are not storage measurements here.
Yugabyte.limitation.1: Local statistics cover the connected YB-TServer. Configured cluster views add other nodes; skipped nodes and independent counter resets prevent complete comparisons.
Yugabyte.limitation.2: Captured generic plans use this login's current role and settings. Actual DocDB RPCs and rows scanned require EXPLAIN ANALYZE with DIST.
Yugabyte.limitation.3: An index's sharding keys affect distributed reads and write hotspots. Suggested ordered keys use explicit ASC/DESC; review partitioning before deployment.
Yugabyte.limitations.0: YSQL uses DocDB/RocksDB over Raft. PostgreSQL heap pages, buffer-cache ratios, dead tuples, vacuum and XID ages are not storage measurements here.
Yugabyte.limitations.1: Local statistics cover the connected YB-TServer. Configured cluster views add other nodes; skipped nodes and independent counter resets prevent complete comparisons.
Yugabyte.limitations.2: Captured generic plans use this login's current role and settings. Actual DocDB RPCs and rows scanned require EXPLAIN ANALYZE with DIST.
Yugabyte.limitations.3: An index's sharding keys affect distributed reads and write hotspots. Suggested ordered keys use explicit ASC/DESC; review partitioning before deployment.
Yugabyte.refusal.deadlocks: DocDB's distributed deadlock detection is not PostgreSQL's heap lock accounting. A PostgreSQL deadlock counter does not establish this database's distributed deadlocks.
Yugabyte.refusal.deadlocks.hint: Correlate YSQL errors and the YB-TServer logs for the affected transaction.
Yugabyte.refusal.last_actual_plan: YSQL statement totals do not retain the measured plan of every past execution
Yugabyte.refusal.last_actual_plan.hint: Analyze a representative statement to capture its actual DocDB work.
Yugabyte.refusal.operator_progress: YSQL does not expose live row counters for every plan operator
Yugabyte.refusal.operator_progress.hint: Use a measured plan and native statement/operation progress.
Yugabyte.refused.sessions: YugabyteDB cannot provide sessions
Yugabyte.refused.wait_sample: YugabyteDB cannot provide wait samples
Yugabyte.refused.deadlocks: DocDB's distributed deadlock detection is not PostgreSQL's heap lock accounting. A PostgreSQL deadlock counter does not establish this database's distributed deadlocks.
Yugabyte.refused.deadlocks.hint: Correlate YSQL errors and the YB-TServer logs for the affected transaction.
Yugabyte.refused.measures_runs: YugabyteDB cannot provide measured runs
Yugabyte.refused.plan_estimates: YugabyteDB cannot provide plan estimates
Yugabyte.refused.writes: YugabyteDB cannot provide writes
Yugabyte.refused.periods: YugabyteDB cannot provide periods
Yugabyte.refused.last_actual_plan: YSQL statement totals do not retain the measured plan of every past execution
Yugabyte.refused.last_actual_plan.hint: Analyze a representative statement to capture its actual DocDB work.
Yugabyte.refused.operator_progress: YSQL does not expose live row counters for every plan operator
Yugabyte.refused.operator_progress.hint: Use a measured plan and native statement/operation progress.
Yugabyte.refused.sign_in: YugabyteDB cannot provide sign-in
Yugabyte.metrics.planning_label: Planning
Yugabyte.metrics.write_rows_label: Some("Rows changed per run")
Yugabyte.metrics.log_name: log
Yugabyte.metrics.log_label: Log written
Yugabyte.metrics.missing_costs_note.YugabyteDB: Some("YugabyteDB keeps no error count or lock time per statement; Live shows blocking as it happens.")
Yugabyte.metrics.errors_note.YugabyteDB: None
Yugabyte.metrics.runtime_note.YugabyteDB: Some("YSQL keeps aggregate run-time statistics in the recorded node or cluster scope. Their spread does not identify which parameter values, tablets, RPCs or retries caused slow runs.")
Yugabyte.metrics.runtime_histogram_note.YugabyteDB: None
Yugabyte.metrics.runtime_cache_note.YugabyteDB: None
Yugabyte.session_advice.discard: Some("This resets session state and is commonly sent by a driver or connection pool; the statement statistics do not identify its caller or pooling mode. Inspect the application's connection lifecycle and the driver or pool configuration, and benchmark any change against the measured absolute cost shown here. Keep resets that prevent session state leaking between users. Npgsql arranges its reset on close and sends it on reuse; PgBouncer normally resets on release in session pooling, and normally does not in transaction pooling. Consider changing reset behavior only after verifying that every caller safely handles retained session state.")
Yugabyte.session_advice.reset: Some("This resets session state and is commonly sent by a driver or connection pool; the statement statistics do not identify its caller or pooling mode. Inspect the application's connection lifecycle and the driver or pool configuration, and benchmark any change against the measured absolute cost shown here. Keep resets that prevent session state leaking between users. Npgsql arranges its reset on close and sends it on reuse; PgBouncer normally resets on release in session pooling, and normally does not in transaction pooling. Consider changing reset behavior only after verifying that every caller safely handles retained session state.")
Yugabyte.session_advice.set: None
Yugabyte.deadlock_limitation: Some("DocDB's distributed deadlock detection is not PostgreSQL's heap lock accounting. A PostgreSQL deadlock counter does not establish this database's distributed deadlocks.")
Yugabyte.unused_index_advice.0.None: Save its native definition and sharding keys before dropping it. Check a complete cluster-wide business cycle of usage, and measure the affected queries after the change.
Yugabyte.unused_index_advice.0.Some("hypertable"): Save its native definition and sharding keys before dropping it. Check a complete cluster-wide business cycle of usage, and measure the affected queries after the change.
Yugabyte.unused_index_advice.0.Some("timescale-chunk"): Save its native definition and sharding keys before dropping it. Check a complete cluster-wide business cycle of usage, and measure the affected queries after the change.
Yugabyte.change_setting.0.work_mem: Set work_mem for new YSQL connections to this database. | Some("ALTER DATABASE shop SET work_mem = '64MB';")
Yugabyte.database_setting.0.work_mem: Some(("Set work_mem for new YSQL connections to this database.", Some("ALTER DATABASE shop SET work_mem = '64MB';")))
Yugabyte.change_setting.0.max_connections: Configure the supported YSQL or DocDB parameter max_connections through YugabyteDB's administration tools; review its scope and restart requirements. | None
Yugabyte.database_setting.0.max_connections: None
Yugabyte.wait_not_per_statement.0.io: DocDB RPC and storage wait timings may overlap; they are not exclusive shares of statement execution time. Use their native metrics or sample active sessions.
Yugabyte.wait_not_per_statement.0.lock: DocDB RPC and storage wait timings may overlap; they are not exclusive shares of statement execution time. Use their native metrics or sample active sessions.
Yugabyte.wait_not_per_statement.0.cpu: DocDB RPC and storage wait timings may overlap; they are not exclusive shares of statement execution time. Use their native metrics or sample active sessions.
Yugabyte.wait_not_per_statement.0.memory: DocDB RPC and storage wait timings may overlap; they are not exclusive shares of statement execution time. Use their native metrics or sample active sessions.
Yugabyte.index_advice_sources.0.true: Some("Candidates come from YSQL plans, the index catalog and foreign keys; DocDB does not publish missing-index requests.")
Yugabyte.index_advice_sources.0.false: Some("Without statement statistics, candidates come from foreign keys and the index catalog.")
Yugabyte.refresh_statistics: Refresh YSQL's optimizer statistics. This does not vacuum or compact DocDB storage. | Some("ANALYZE public.orders;") | Some("ANALYZE samples table data and can issue distributed reads.")
Mysql.plan_format.Mysql: EXPLAIN FORMAT=JSON, EXPLAIN ANALYZE or FORMAT=TREE
Mysql.plan_format.Mariadb: EXPLAIN FORMAT=JSON or ANALYZE FORMAT=JSON
Mysql.accepts: A mysql://, mariadb:// or JDBC URL, a .NET connection string (Server=…;Uid=…), or host:port/database.
Mysql.waiting_state: active
Mysql.limitation.0: Statement digests keep aggregate totals, not an application's request trace, transaction boundaries or the plan of every historical execution. Culprit compares only intervals its scans or the slow log record.
Mysql.limitation.1: InnoDB status keeps only the latest deadlock. A complete incident history requires the error log; an empty or old latest report does not prove that no other deadlocks happened.
Mysql.limitation.2: MySQL and MariaDB differ in plan details, instrumentation and syntax. Runtime measurements, metadata-lock visibility and replication diagnostics depend on the product, version and permissions.
Mysql.limitations.0: Statement digests keep aggregate totals, not an application's request trace, transaction boundaries or the plan of every historical execution. Culprit compares only intervals its scans or the slow log record.
Mysql.limitations.1: InnoDB status keeps only the latest deadlock. A complete incident history requires the error log; an empty or old latest report does not prove that no other deadlocks happened.
Mysql.limitations.2: MySQL and MariaDB differ in plan details, instrumentation and syntax. Runtime measurements, metadata-lock visibility and replication diagnostics depend on the product, version and permissions.
Mysql.refusal.last_actual_plan: MySQL and MariaDB don't keep the actual plan of past runs
Mysql.refusal.last_actual_plan.hint: Measure it instead: Analyze runs the statement's recorded text once inside a read-only transaction.
Mysql.refusal.operator_progress: MySQL reports a running statement's progress, not each step's
Mysql.refusal.operator_progress.hint: Its plan doesn't change while it runs: ask for the plan once.
Mysql.refused.sessions: MySQL can't do this (sessions)
Mysql.refused.wait_sample: MySQL can't do this (wait sample)
Mysql.refused.deadlocks: MySQL can't do this (deadlocks)
Mysql.refused.measures_runs: MySQL can't do this (measures runs)
Mysql.refused.plan_estimates: MySQL can't do this (plan estimates)
Mysql.refused.writes: MySQL can't do this (writes)
Mysql.refused.periods: MySQL can't do this (periods)
Mysql.refused.last_actual_plan: MySQL and MariaDB don't keep the actual plan of past runs
Mysql.refused.last_actual_plan.hint: Measure it instead: Analyze runs the statement's recorded text once inside a read-only transaction.
Mysql.refused.operator_progress: MySQL reports a running statement's progress, not each step's
Mysql.refused.operator_progress.hint: Its plan doesn't change while it runs: ask for the plan once.
Mysql.refused.sign_in: MySQL can't do this (sign in)
Mysql.metrics.planning_label: Planning
Mysql.metrics.write_rows_label: None
Mysql.metrics.log_name: log
Mysql.metrics.log_label: Log written
Mysql.metrics.missing_costs_note.MySQL: Some("MySQL keeps no planning time or redo bytes per statement.")
Mysql.metrics.errors_note.MySQL: None
Mysql.metrics.runtime_note.MySQL: Some("This MySQL scan includes no run-time histogram; the lane shows fastest, average and slowest. Histogram availability depends on the product, server version and collection permissions; the slow query log can add individual logged runs.")
Mysql.metrics.runtime_histogram_note.MySQL: Some("MySQL keeps a run-time histogram per statement, so the bars show how many runs took how long.")
Mysql.metrics.runtime_cache_note.MySQL: None
Mysql.metrics.missing_costs_note.MariaDB: Some("MariaDB keeps no planning time or redo bytes per statement.")
Mysql.metrics.errors_note.MariaDB: None
Mysql.metrics.runtime_note.MariaDB: Some("This MariaDB scan includes no run-time histogram; the lane shows fastest, average and slowest. Histogram availability depends on the product, server version and collection permissions; the slow query log can add individual logged runs.")
Mysql.metrics.runtime_histogram_note.MariaDB: Some("MariaDB keeps a run-time histogram per statement, so the bars show how many runs took how long.")
Mysql.metrics.runtime_cache_note.MariaDB: None
Mysql.session_advice.discard: None
Mysql.session_advice.reset: None
Mysql.session_advice.set: None
Mysql.deadlock_limitation: Some("InnoDB keeps only its most recent deadlock. Turn on innodb_print_all_deadlocks to write every one to the error log.")
Mysql.unused_index_advice.0.None: Make it invisible first: the optimizer stops using it but it stays maintained, and `ALTER INDEX … VISIBLE` brings it back instantly. Drop it after a full business cycle without complaints.
Mysql.unused_index_advice.0.Some("hypertable"): Make it invisible first: the optimizer stops using it but it stays maintained, and `ALTER INDEX … VISIBLE` brings it back instantly. Drop it after a full business cycle without complaints.
Mysql.unused_index_advice.0.Some("timescale-chunk"): Make it invisible first: the optimizer stops using it but it stays maintained, and `ALTER INDEX … VISIBLE` brings it back instantly. Drop it after a full business cycle without complaints.
Mysql.change_setting.0.work_mem: Set work_mem to 64MB; SET PERSIST keeps it across restarts. | Some("SET PERSIST work_mem = '64MB';")
Mysql.database_setting.0.work_mem: None
Mysql.change_setting.0.max_connections: Set max_connections to 200; SET PERSIST keeps it across restarts. | Some("SET PERSIST max_connections = 200;")
Mysql.database_setting.0.max_connections: None
Mysql.wait_not_per_statement.0.io: MySQL doesn't attribute this wait to statements; it is a server-wide total.
Mysql.wait_not_per_statement.0.lock: No statement recorded this in performance_schema.
Mysql.wait_not_per_statement.0.cpu: No statement recorded this in performance_schema.
Mysql.wait_not_per_statement.0.memory: MySQL doesn't attribute this wait to statements; it is a server-wide total.
Mysql.index_advice_sources.0.true: Some("MySQL doesn't record missing-index requests. Candidates come from captured plans, foreign keys, and rows read without an index (performance_schema).")
Mysql.index_advice_sources.0.false: Some("MySQL doesn't record missing-index requests. Candidates come from captured plans, foreign keys, and rows read without an index (performance_schema).")
Mysql.unused_index_advice.1.None: Save its definition, then drop it: MySQL 5.7 can't hide an index (invisible indexes arrived in 8.0).
Mysql.unused_index_advice.1.Some("hypertable"): Save its definition, then drop it: MySQL 5.7 can't hide an index (invisible indexes arrived in 8.0).
Mysql.unused_index_advice.1.Some("timescale-chunk"): Save its definition, then drop it: MySQL 5.7 can't hide an index (invisible indexes arrived in 8.0).
Mysql.change_setting.1.work_mem: Set work_mem to 64MB now, and in the server's option file (my.cnf) so it survives a restart. | Some("SET GLOBAL work_mem = '64MB';")
Mysql.database_setting.1.work_mem: None
Mysql.change_setting.1.max_connections: Set max_connections to 200 now, and in the server's option file (my.cnf) so it survives a restart. | Some("SET GLOBAL max_connections = 200;")
Mysql.database_setting.1.max_connections: None
Mysql.wait_not_per_statement.1.io: MySQL doesn't attribute this wait to statements; it is a server-wide total.
Mysql.wait_not_per_statement.1.lock: No statement recorded this in performance_schema.
Mysql.wait_not_per_statement.1.cpu: No statement recorded this in performance_schema.
Mysql.wait_not_per_statement.1.memory: MySQL doesn't attribute this wait to statements; it is a server-wide total.
Mysql.index_advice_sources.1.true: Some("MySQL doesn't record missing-index requests. Candidates come from the plans of logged statements and foreign keys; performance_schema would add rows read without an index.")
Mysql.index_advice_sources.1.false: Some("MySQL doesn't record missing-index requests. Candidates come from the plans of logged statements and foreign keys; performance_schema would add rows read without an index.")
Mysql.unused_index_advice.2.None: Have the optimizer ignore it first: it stays maintained, and `ALTER INDEX … NOT IGNORED` brings it back instantly. Drop it after a full business cycle without complaints.
Mysql.unused_index_advice.2.Some("hypertable"): Have the optimizer ignore it first: it stays maintained, and `ALTER INDEX … NOT IGNORED` brings it back instantly. Drop it after a full business cycle without complaints.
Mysql.unused_index_advice.2.Some("timescale-chunk"): Have the optimizer ignore it first: it stays maintained, and `ALTER INDEX … NOT IGNORED` brings it back instantly. Drop it after a full business cycle without complaints.
Mysql.change_setting.2.work_mem: Set work_mem to 64MB now, and in the server's option file (my.cnf) so it survives a restart. | Some("SET GLOBAL work_mem = '64MB';")
Mysql.database_setting.2.work_mem: None
Mysql.change_setting.2.max_connections: Set max_connections to 200 now, and in the server's option file (my.cnf) so it survives a restart. | Some("SET GLOBAL max_connections = 200;")
Mysql.database_setting.2.max_connections: None
Mysql.wait_not_per_statement.2.io: MariaDB doesn't attribute this wait to statements; it is a server-wide total.
Mysql.wait_not_per_statement.2.lock: No statement recorded this in performance_schema.
Mysql.wait_not_per_statement.2.cpu: No statement recorded this in performance_schema.
Mysql.wait_not_per_statement.2.memory: MariaDB doesn't attribute this wait to statements; it is a server-wide total.
Mysql.index_advice_sources.2.true: Some("MariaDB doesn't record missing-index requests. Candidates come from captured plans, foreign keys, and rows read without an index (performance_schema).")
Mysql.index_advice_sources.2.false: Some("MariaDB doesn't record missing-index requests. Candidates come from captured plans, foreign keys, and rows read without an index (performance_schema).")
Mysql.unused_index_advice.3.None: Save its definition, then drop it: this MariaDB version can't hide an index (ignored indexes arrived in 10.6).
Mysql.unused_index_advice.3.Some("hypertable"): Save its definition, then drop it: this MariaDB version can't hide an index (ignored indexes arrived in 10.6).
Mysql.unused_index_advice.3.Some("timescale-chunk"): Save its definition, then drop it: this MariaDB version can't hide an index (ignored indexes arrived in 10.6).
Mysql.change_setting.3.work_mem: Set work_mem to 64MB now, and in the server's option file (my.cnf) so it survives a restart. | Some("SET GLOBAL work_mem = '64MB';")
Mysql.database_setting.3.work_mem: None
Mysql.change_setting.3.max_connections: Set max_connections to 200 now, and in the server's option file (my.cnf) so it survives a restart. | Some("SET GLOBAL max_connections = 200;")
Mysql.database_setting.3.max_connections: None
Mysql.wait_not_per_statement.3.io: MariaDB doesn't attribute this wait to statements; it is a server-wide total.
Mysql.wait_not_per_statement.3.lock: No statement recorded this in performance_schema.
Mysql.wait_not_per_statement.3.cpu: No statement recorded this in performance_schema.
Mysql.wait_not_per_statement.3.memory: MariaDB doesn't attribute this wait to statements; it is a server-wide total.
Mysql.index_advice_sources.3.true: Some("MariaDB doesn't record missing-index requests. Candidates come from the plans of logged statements and foreign keys; performance_schema would add rows read without an index.")
Mysql.index_advice_sources.3.false: Some("MariaDB doesn't record missing-index requests. Candidates come from the plans of logged statements and foreign keys; performance_schema would add rows read without an index.")
Mysql.unused_index_advice.4.None: Save its definition before dropping it. Index-hiding support is not established for this server version.
Mysql.unused_index_advice.4.Some("hypertable"): Save its definition before dropping it. Index-hiding support is not established for this server version.
Mysql.unused_index_advice.4.Some("timescale-chunk"): Save its definition before dropping it. Index-hiding support is not established for this server version.
Mysql.change_setting.4.work_mem: Set work_mem to 64MB; SET PERSIST keeps it across restarts. | Some("SET PERSIST work_mem = '64MB';")
Mysql.database_setting.4.work_mem: None
Mysql.change_setting.4.max_connections: Set max_connections to 200; SET PERSIST keeps it across restarts. | Some("SET PERSIST max_connections = 200;")
Mysql.database_setting.4.max_connections: None
Mysql.wait_not_per_statement.4.io: MySQL doesn't attribute this wait to statements; it is a server-wide total.
Mysql.wait_not_per_statement.4.lock: No statement recorded this in performance_schema.
Mysql.wait_not_per_statement.4.cpu: No statement recorded this in performance_schema.
Mysql.wait_not_per_statement.4.memory: MySQL doesn't attribute this wait to statements; it is a server-wide total.
Mysql.index_advice_sources.4.true: Some("MySQL doesn't record missing-index requests. Candidates come from the plans of logged statements and foreign keys; performance_schema would add rows read without an index.")
Mysql.index_advice_sources.4.false: Some("MySQL doesn't record missing-index requests. Candidates come from the plans of logged statements and foreign keys; performance_schema would add rows read without an index.")
Mysql.refresh_statistics: Recompute the table's statistics. | Some("ANALYZE TABLE public.orders;") | None
Sqlite.plan_format.Sqlite: EXPLAIN QUERY PLAN, as the sqlite3 shell prints it
Sqlite.accepts: The database file. Culprit opens it read-only and needs nothing from your app.
Sqlite.waiting_state: active
Sqlite.limitation.0: SQLite file inspection cannot see the application's connections, transaction owners, foreign_keys, busy_timeout or synchronous settings. Configure and inspect those on the application's own connections.
Sqlite.limitation.1: Plans are obtained with Culprit's SQLite library, which may differ from the application's version, modules or build options. Logged placeholders are planned with NULL values, so value-dependent access paths may differ.
Sqlite.limitation.2: EXPLAIN QUERY PLAN records access paths, not estimated or measured rows, costs or timing. Query durations and errors require a supported application log; request traces need request identifiers in that telemetry.
Sqlite.limitation.3: A WAL file can retain allocated space after checkpointing. Its size or growth alone does not establish checkpoint starvation or identify a reader holding a snapshot.
Sqlite.limitations.0: SQLite file inspection cannot see the application's connections, transaction owners, foreign_keys, busy_timeout or synchronous settings. Configure and inspect those on the application's own connections.
Sqlite.limitations.1: Plans are obtained with Culprit's SQLite library, which may differ from the application's version, modules or build options. Logged placeholders are planned with NULL values, so value-dependent access paths may differ.
Sqlite.limitations.2: EXPLAIN QUERY PLAN records access paths, not estimated or measured rows, costs or timing. Query durations and errors require a supported application log; request traces need request identifiers in that telemetry.
Sqlite.limitations.3: A WAL file can retain allocated space after checkpointing. Its size or growth alone does not establish checkpoint starvation or identify a reader holding a snapshot.
Sqlite.refusal.deadlocks: SQLite records no deadlocks: one writer at a time holds the file
Sqlite.refusal.deadlocks.hint: A transaction that can't get the write lock waits busy_timeout, then gets SQLITE_BUSY in the application: its logs say when.
Sqlite.refusal.last_actual_plan: SQLite's plans have no actual rows or timings: EXPLAIN QUERY PLAN shows the access path only
Sqlite.refusal.last_actual_plan.hint: Use the estimated plan; time the statement in the application to see how long it takes.
Sqlite.refusal.measures_runs: SQLite's plans have no actual rows or timings: EXPLAIN QUERY PLAN shows the access path only
Sqlite.refusal.measures_runs.hint: Use the estimated plan; time the statement in the application to see how long it takes.
Sqlite.refusal.operator_progress: SQLite has no server, so no sessions to show
Sqlite.refusal.operator_progress.hint: A writer that has to wait gets SQLITE_BUSY in the application: its logs say when.
Sqlite.refusal.periods: SQLite keeps no statement history to compare two periods of
Sqlite.refusal.periods.hint: Add the log your framework writes to the connection, scan before and after the change, and compare the two reports' queries.
Sqlite.refusal.plan_estimates: SQLite's plans have no actual rows or timings: EXPLAIN QUERY PLAN shows the access path only
Sqlite.refusal.plan_estimates.hint: Use the estimated plan; time the statement in the application to see how long it takes.
Sqlite.refusal.sessions: SQLite has no server, so no sessions to show
Sqlite.refusal.sessions.hint: A writer that has to wait gets SQLITE_BUSY in the application: its logs say when.
Sqlite.refusal.sign_in: A SQLite file takes no sign-in: Culprit opens it read-only from disk
Sqlite.refusal.wait_sample: SQLite has no server, so nothing records what it waits on
Sqlite.refusal.wait_sample.hint: Time spent waiting for the write lock shows up in the application as SQLITE_BUSY or slow commits.
Sqlite.refusal.writes: Culprit opens SQLite files read-only, so it never changes one
Sqlite.refusal.writes.hint: Run it with the sqlite3 shell or from the application, then scan again to see the effect.
Sqlite.refused.sessions: SQLite has no server, so no sessions to show
Sqlite.refused.sessions.hint: A writer that has to wait gets SQLITE_BUSY in the application: its logs say when.
Sqlite.refused.wait_sample: SQLite has no server, so nothing records what it waits on
Sqlite.refused.wait_sample.hint: Time spent waiting for the write lock shows up in the application as SQLITE_BUSY or slow commits.
Sqlite.refused.deadlocks: SQLite records no deadlocks: one writer at a time holds the file
Sqlite.refused.deadlocks.hint: A transaction that can't get the write lock waits busy_timeout, then gets SQLITE_BUSY in the application: its logs say when.
Sqlite.refused.measures_runs: SQLite's plans have no actual rows or timings: EXPLAIN QUERY PLAN shows the access path only
Sqlite.refused.measures_runs.hint: Use the estimated plan; time the statement in the application to see how long it takes.
Sqlite.refused.plan_estimates: SQLite's plans have no actual rows or timings: EXPLAIN QUERY PLAN shows the access path only
Sqlite.refused.plan_estimates.hint: Use the estimated plan; time the statement in the application to see how long it takes.
Sqlite.refused.writes: Culprit opens SQLite files read-only, so it never changes one
Sqlite.refused.writes.hint: Run it with the sqlite3 shell or from the application, then scan again to see the effect.
Sqlite.refused.periods: SQLite keeps no statement history to compare two periods of
Sqlite.refused.periods.hint: Add the log your framework writes to the connection, scan before and after the change, and compare the two reports' queries.
Sqlite.refused.last_actual_plan: SQLite's plans have no actual rows or timings: EXPLAIN QUERY PLAN shows the access path only
Sqlite.refused.last_actual_plan.hint: Use the estimated plan; time the statement in the application to see how long it takes.
Sqlite.refused.operator_progress: SQLite has no server, so no sessions to show
Sqlite.refused.operator_progress.hint: A writer that has to wait gets SQLITE_BUSY in the application: its logs say when.
Sqlite.refused.sign_in: A SQLite file takes no sign-in: Culprit opens it read-only from disk
Sqlite.metrics.planning_label: Planning
Sqlite.metrics.write_rows_label: Some("Rows returned per run")
Sqlite.metrics.log_name: log
Sqlite.metrics.log_label: Log written
Sqlite.metrics.missing_costs_note.SQLite: None
Sqlite.metrics.errors_note.SQLite: None
Sqlite.metrics.runtime_note.SQLite: Some("SQLite keeps no run times; these come from the query log Culprit read, one entry per logged run.")
Sqlite.metrics.runtime_histogram_note.SQLite: None
Sqlite.metrics.runtime_cache_note.SQLite: None
Sqlite.session_advice.discard: None
Sqlite.session_advice.reset: None
Sqlite.session_advice.set: None
Sqlite.deadlock_limitation: None
Sqlite.unused_index_advice.0.None: Drop it: SQLite can't hide an index first, so keep its CREATE INDEX statement to put it back if a query slows down.
Sqlite.unused_index_advice.0.Some("hypertable"): Drop it: SQLite can't hide an index first, so keep its CREATE INDEX statement to put it back if a query slows down.
Sqlite.unused_index_advice.0.Some("timescale-chunk"): Drop it: SQLite can't hide an index first, so keep its CREATE INDEX statement to put it back if a query slows down.
Sqlite.change_setting.0.work_mem: Set work_mem to 64MB with PRAGMA. If it is a per-connection pragma, the application has to set it each time it opens the file. | Some("PRAGMA work_mem = 64MB;")
Sqlite.database_setting.0.work_mem: None
Sqlite.change_setting.0.max_connections: Set max_connections to 200 with PRAGMA. If it is a per-connection pragma, the application has to set it each time it opens the file. | Some("PRAGMA max_connections = 200;")
Sqlite.database_setting.0.max_connections: None
Sqlite.wait_not_per_statement.0.io: SQLite keeps no waits.
Sqlite.wait_not_per_statement.0.lock: SQLite keeps no waits.
Sqlite.wait_not_per_statement.0.cpu: SQLite keeps no waits.
Sqlite.wait_not_per_statement.0.memory: SQLite keeps no waits.
Sqlite.index_advice_sources.0.true: Some("SQLite doesn't record missing-index requests. Candidates come from the indexes it builds for itself on every run of the logged statements, and foreign keys.")
Sqlite.index_advice_sources.0.false: Some("SQLite keeps no history of the statements it ran, so candidates come from foreign keys only. With the application's query log on the connection, the indexes SQLite builds for itself on every run join them.")
Sqlite.refresh_statistics: Run ANALYZE, so the planner knows each table's size and how selective each index is. | Some("ANALYZE public.orders;") | Some("ANALYZE reads the indexes and briefly takes the write lock to store what it found.")
Yugabyte.session_target: Select a node-qualified YSQL session from Live"#;

#[test]
fn setting_defaults_are_explicit_engine_facts() {
    for engine in Engine::ALL {
        for (name, default) in [
            ("cost threshold for parallelism", "5"),
            ("max degree of parallelism", "0"),
            ("optimize for ad hoc workloads", "0"),
            ("max server memory (MB)", "2147483647"),
        ] {
            assert_eq!(engine.spec().default_setting_value(name), (engine == Engine::Mssql).then_some(default));
        }
        assert_eq!(engine.spec().default_setting_value("unrecognized setting"), None);
    }
}

#[test]
fn evidence_presentation_has_explicit_native_semantics_for_every_engine() {
    use super::presentation::StatisticsFreshness::*;
    let cases = [
        (Engine::Mssql, PerObject, true, true, true),
        (Engine::Postgres, PerTable, false, false, false),
        (Engine::Yugabyte, Unavailable, false, false, false),
        (Engine::Mysql, PerTable, false, true, false),
        (Engine::Sqlite, Unavailable, false, false, false),
    ];
    assert_eq!(cases.map(|c| c.0), Engine::ALL);
    for (engine, statistics, windowed, deadlocks, retained) in cases {
        let facts = engine.facts().evidence_presentation;
        assert_eq!(facts.statistics_freshness, statistics);
        assert_eq!(facts.statistics_absence_known, engine == Engine::Postgres);
        assert_eq!(
            (facts.workload_windowed_by_default, facts.deadlock_time_window, facts.retained_actual_plans),
            (windowed, deadlocks, retained)
        );
        for key in [&facts.storage_about, &facts.storage_untimed, &facts.health_empty, &facts.generic_plan_context] {
            assert_ne!(say(key, &[]), *key, "missing presentation message for {engine:?}");
        }
    }
}
