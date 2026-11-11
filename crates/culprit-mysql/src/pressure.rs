//! Pressure signals and the "right now" wait sample: global status counters, InnoDB metrics and the oldest open
//! transaction, plus two readings of performance_schema's wait totals a few seconds apart.

use std::collections::{BTreeMap, HashMap};
use std::time::{Duration, Instant};

use culprit_core::{
    CleanupHolder, Coverage, Engine, OpenTransaction, Pressure, Result, Signal, SignalScope, SignalUnit,
    TransactionActivity, WaitSample, WaitStat, format, t,
};
use mysql_async::Value;

use crate::connect::TAG;
use crate::rows::{query, query_one, query_with};
use crate::sources;

const PICOS_PER_MS: f64 = 1e9;

/// The shared category for a performance_schema wait event.
pub fn category(event: &str) -> &'static str {
    let e = event.to_ascii_lowercase();
    if e.starts_with("wait/io/file/innodb/innodb_log_file") || e.starts_with("wait/io/file/innodb/innodb_redo") {
        return "Log";
    }
    if e.starts_with("wait/io/file") {
        return "IO";
    }
    if e.starts_with("wait/io/table") {
        return "Table IO";
    }
    if e.starts_with("wait/io/socket") {
        return "Network";
    }
    if e.starts_with("wait/lock") {
        return "Lock";
    }
    if e.starts_with("wait/synch") {
        return "Latch";
    }
    "Other"
}

/// Event name → (wait ms, count) from performance_schema's server-wide wait totals.
/// Wait totals since startup. `wait/io/table/sql/handler` is left out: it times row reads inside the storage engine,
/// the statements' own work rather than waiting, and would otherwise dwarf every real wait.
async fn wait_totals(conn: &mut impl crate::read::QueryRead) -> Result<HashMap<String, (f64, f64)>> {
    Ok(query(
        conn,
        "SELECT EVENT_NAME AS event, COUNT_STAR AS tasks, SUM_TIMER_WAIT AS wait_ps
           FROM performance_schema.events_waits_summary_global_by_event_name
          WHERE SUM_TIMER_WAIT > 0 AND EVENT_NAME NOT IN ('idle', 'wait/io/table/sql/handler')",
    )
    .await?
    .iter()
    .map(|r| (r.string("event"), (r.number("wait_ps").unwrap_or(0.0) / PICOS_PER_MS, r.number("tasks").unwrap_or(0.0))))
    .collect())
}

/// Server-wide wait totals since startup, folded into categories, each with its biggest event.
pub async fn waits(conn: &mut impl crate::read::QueryRead) -> Result<Vec<WaitStat>> {
    Ok(fold(wait_totals(conn).await?.into_iter().map(|(e, (ms, n))| (e, ms, n))))
}

fn fold(items: impl IntoIterator<Item = (String, f64, f64)>) -> Vec<WaitStat> {
    let mut by_category: BTreeMap<&'static str, (f64, f64, String, f64)> = BTreeMap::new();
    for (event, ms, tasks) in items {
        if ms <= 0.0 {
            continue;
        }
        let entry = by_category.entry(category(&event)).or_default();
        entry.0 += ms;
        entry.1 += tasks;
        if ms > entry.3 {
            entry.2 = event;
            entry.3 = ms;
        }
    }
    let mut out: Vec<WaitStat> = by_category
        .into_iter()
        .map(|(category, (ms, tasks, top, _))| WaitStat {
            category: category.into(),
            wait_type: Some(top),
            wait_ms: ms,
            tasks: Some(tasks),
        })
        .collect();
    out.sort_by(|a, b| b.wait_ms.total_cmp(&a.wait_ms));
    out
}

async fn status(conn: &mut impl crate::read::QueryRead) -> Result<HashMap<String, f64>> {
    crate::vars::status(
        conn,
        &[
            "Uptime",
            "Innodb_buffer_pool_reads",
            "Innodb_buffer_pool_read_requests",
            "Innodb_buffer_pool_wait_free",
            "Created_tmp_disk_tables",
            "Created_tmp_tables",
            "Questions",
            "Threads_running",
            "Threads_connected",
            "Max_used_connections",
            "Innodb_row_lock_waits",
            "Innodb_row_lock_time",
            "Innodb_row_lock_time_avg",
        ],
    )
    .await
}

/// Seconds since connections peaked at Max_used_connections (MySQL 5.7.5+; MariaDB doesn't keep the time). The status
/// value is written in the session's time zone, which Culprit sets to UTC.
async fn peak_connections_ago(conn: &mut impl crate::read::QueryRead) -> Option<f64> {
    let rows = query(conn, "SHOW GLOBAL STATUS LIKE 'Max_used_connections_time'").await.ok()?;
    let at = rows.first().and_then(|r| r.text("Value").or_else(|| r.text("value")))?;
    let rows = query_with(conn, "SELECT TIMESTAMPDIFF(SECOND, ?, UTC_TIMESTAMP()) AS ago", vec![Value::from(at)])
        .await
        .ok()?;
    rows.first().and_then(|r| r.number("ago")).filter(|s| *s >= 0.0)
}

/// The oldest transaction that holds InnoDB's purge back, with the connecting program's name when performance_schema
/// has it. Purge is the whole server's, so a transaction in any database counts; what holds it is a read view, which a
/// REPEATABLE READ or SERIALIZABLE transaction keeps until it ends and a READ COMMITTED one only while a statement
/// runs. A READ COMMITTED transaction sitting idle holds no read view, so it isn't the one.
fn oldest_sql(with_program: bool) -> String {
    let (column, join) = match with_program {
        true => (
            "a.ATTR_VALUE AS program",
            "LEFT JOIN performance_schema.session_connect_attrs a
                    ON a.PROCESSLIST_ID = t.trx_mysql_thread_id AND a.ATTR_NAME = 'program_name'",
        ),
        false => ("NULL AS program", ""),
    };
    format!(
        "SELECT CAST(t.trx_mysql_thread_id AS CHAR) AS session, TIMESTAMPDIFF(SECOND, t.trx_started, NOW()) AS seconds,
                p.COMMAND AS command, p.DB AS db, t.trx_rows_modified AS modified, {column}
           FROM information_schema.INNODB_TRX t
           LEFT JOIN information_schema.PROCESSLIST p ON p.ID = t.trx_mysql_thread_id
           {join}
          WHERE t.trx_mysql_thread_id <> CONNECTION_ID()
            AND (t.trx_isolation_level IN ('REPEATABLE READ', 'SERIALIZABLE') OR p.COMMAND <> 'Sleep')
          ORDER BY t.trx_started LIMIT 1"
    )
}

/// Culprit's own connections: those named `program_name = culprit` where performance_schema shows connection
/// attributes, otherwise this session and any running a tagged statement.
async fn own_sessions(conn: &mut impl crate::read::QueryRead) -> f64 {
    let named = "SELECT COUNT(DISTINCT PROCESSLIST_ID) AS n FROM performance_schema.session_connect_attrs
                  WHERE ATTR_NAME = 'program_name' AND ATTR_VALUE = 'culprit'";
    if let Ok(rows) = query(conn, named).await
        && let Some(n) = rows.first().and_then(|r| r.number("n")).filter(|n| *n >= 1.0)
    {
        return n;
    }
    let tagged = "SELECT COUNT(*) AS n FROM information_schema.PROCESSLIST WHERE ID = CONNECTION_ID() OR INFO LIKE ?";
    query_with(conn, tagged, vec![Value::from(format!("%{TAG}%"))])
        .await
        .ok()
        .and_then(|rows| rows.first().and_then(|r| r.number("n")))
        .unwrap_or(1.0)
}

/// Statements running right now, counted the way Live counts them: other sessions' work, never Culprit's own (a scan
/// runs several collectors at once, each on its own session, so `Threads_running - 1` still counts some of them).
async fn running_statements(conn: &mut impl crate::read::QueryRead) -> Option<f64> {
    let sql = "SELECT COUNT(*) AS n FROM information_schema.PROCESSLIST
                WHERE ID <> CONNECTION_ID() AND COMMAND NOT IN ('Sleep', 'Daemon', 'Binlog Dump', 'Binlog Dump GTID')
                  AND USER <> 'system user' AND (INFO IS NULL OR INFO NOT LIKE ?)";
    let rows = query_with(conn, sql, vec![Value::from(format!("%{TAG}%"))]).await.ok()?;
    rows.first().and_then(|r| r.number("n"))
}

/// What [`collect`] read: global status, and what the signals beyond it need.
#[derive(Default)]
struct Readings {
    /// SHOW GLOBAL STATUS, when it could be read.
    status: Option<HashMap<String, f64>>,
    /// Other sessions' statements running now.
    running: Option<f64>,
    connections: Option<ConnectionLimit>,
    /// Undo logs purge hasn't removed yet (the history list length).
    history: Option<f64>,
    oldest: Option<OpenTransaction>,
}

/// max_connections, with what the connection signals set against it.
struct ConnectionLimit {
    max: f64,
    /// Culprit's own sessions: this scan opens several.
    own: f64,
    /// Seconds since connections peaked, where the server keeps when.
    peak_ago: Option<f64>,
}

/// The pressure counters, each source read through `Coverage::read` (`crate::sources`).
pub async fn collect(conn: &mut impl crate::read::QueryRead) -> (Pressure, Coverage) {
    let mut coverage = Coverage::default();
    let mut r = Readings { running: running_statements(conn).await, ..Readings::default() };
    r.status = coverage.read(&sources::GLOBAL_STATUS, status(conn).await);
    if let Some(s) = &r.status
        && let Ok(Some(row)) = query_one(conn, "SELECT @@max_connections AS max").await
        && let Some(max) = row.number("max").filter(|m| *m > 0.0)
    {
        let own = own_sessions(conn).await;
        let peak_ago = if s.contains_key("Max_used_connections") { peak_connections_ago(conn).await } else { None };
        r.connections = Some(ConnectionLimit { max, own, peak_ago });
    }
    r.history = query_one(
        conn,
        "SELECT `COUNT` AS n FROM information_schema.INNODB_METRICS WHERE NAME = 'trx_rseg_history_len'",
    )
    .await
    .ok()
    .flatten()
    .and_then(|r| r.number("n"));
    // The program name comes from performance_schema's connection attributes, which a server without it lacks.
    let oldest = match query_one(conn, &oldest_sql(true)).await {
        Ok(found) => Ok(found),
        Err(_) => query_one(conn, &oldest_sql(false)).await,
    };
    if let Some(Some(row)) = coverage.read(&sources::OPEN_TRANSACTIONS, oldest) {
        r.oldest = Some(OpenTransaction {
            session: row.string("session"),
            seconds: row.number("seconds").unwrap_or(0.0),
            program: row.text("program").filter(|s| !s.is_empty()),
            activity: if row.text("command").as_deref() == Some("Sleep") {
                TransactionActivity::IdleInTransaction
            } else {
                TransactionActivity::Running
            },
            holder: CleanupHolder::Session,
            wrote: row.number("modified").map(|m| m > 0.0),
            database: row.text("db").filter(|d| !d.is_empty()),
            ..OpenTransaction::default()
        });
    }
    (pressure_from(r), coverage)
}

/// The pressure signals from what [`collect`] read.
fn pressure_from(r: Readings) -> Pressure {
    let mut p = Pressure::default();
    if let Some(s) = &r.status {
        let get = |k: &str| s.get(k).copied();
        let uptime_s = get("Uptime").filter(|u| *u > 0.0);
        p.uptime_hours = uptime_s.map(|u| u / 3600.0);
        if let (Some(reads), Some(requests)) =
            (get("Innodb_buffer_pool_reads"), get("Innodb_buffer_pool_read_requests").filter(|r| *r > 0.0))
        {
            p.signals.push(
                Signal::new(
                    culprit_core::signal_id::BUFFER_POOL_HIT,
                    culprit_core::message!("provider.mysql.signal.buffer_pool_hit"),
                    (1.0 - reads / requests) * 100.0,
                    SignalUnit::Percent,
                    SignalScope::SinceStartup,
                )
                .graded(&t!("provider.mysql.signal.buffer_pool_hit.healthy"), 99.0, 95.0, false),
            );
            // How much reading from disk that miss rate is: a 98% hit rate can be 1 read a second or 10,000.
            if let Some(u) = uptime_s {
                p.signals.push(Signal::new(
                    culprit_core::signal_id::BUFFER_POOL_READS,
                    culprit_core::message!("provider.mysql.signal.buffer_pool_reads"),
                    reads / u,
                    SignalUnit::PerSecond,
                    SignalScope::SinceStartup,
                ));
            }
        }
        if let Some(waits) = get("Innodb_buffer_pool_wait_free").filter(|w| *w > 0.0) {
            p.signals.push(Signal::new(
                "buffer-pool-wait-free",
                culprit_core::message!("provider.mysql.signal.buffer_pool_wait_free"),
                waits,
                SignalUnit::Count,
                SignalScope::SinceStartup,
            ));
        }
        if let (Some(disk), Some(all)) =
            (get("Created_tmp_disk_tables"), get("Created_tmp_tables").filter(|t| *t > 0.0))
        {
            p.signals.push(
                Signal::new(
                    culprit_core::signal_id::TMP_DISK_TABLES,
                    culprit_core::message!("provider.mysql.signal.tmp_disk_tables"),
                    disk / all * 100.0,
                    SignalUnit::Percent,
                    SignalScope::SinceStartup,
                )
                .graded(&t!("provider.mysql.signal.tmp_disk_tables.healthy"), 10.0, 25.0, true),
            );
        }
        if let (Some(q), Some(u)) = (get("Questions"), uptime_s) {
            p.signals.push(Signal::new(
                "questions",
                culprit_core::message!("provider.mysql.signal.statements"),
                q / u,
                SignalUnit::PerSecond,
                SignalScope::SinceStartup,
            ));
        }
        // The process list without Culprit's sessions; the counter (minus this session) if it can't be read.
        if let Some(running) = r.running.or_else(|| get("Threads_running").map(|r| (r - 1.0).max(0.0))) {
            p.signals.push(Signal::new(
                "threads-running",
                culprit_core::message!("provider.mysql.signal.statements_running"),
                running,
                SignalUnit::Count,
                SignalScope::Now,
            ));
        }
        if let (Some(waits), Some(u)) = (get("Innodb_row_lock_waits"), uptime_s) {
            // Under an hour of uptime, a rate per hour would be an extrapolation: report the plain count.
            let hours = u / 3600.0;
            let (label, value, unit) = if hours >= 1.0 {
                (culprit_core::message!("provider.mysql.signal.row_lock_waits"), waits / hours, SignalUnit::PerHour)
            } else {
                (culprit_core::message!("provider.mysql.signal.row_lock_waits_since_start"), waits, SignalUnit::Count)
            };
            p.signals.push(Signal::new(
                culprit_core::signal_id::ROW_LOCK_WAITS,
                &label,
                value,
                unit,
                SignalScope::SinceStartup,
            ));
        }
        // The whole server's time waiting for row locks since it started (every database's).
        if let Some(total) = get("Innodb_row_lock_time") {
            p.signals.push(Signal::new(
                culprit_core::signal_id::ROW_LOCK_TIME,
                culprit_core::message!("provider.mysql.signal.row_lock_time"),
                total,
                SignalUnit::Milliseconds,
                SignalScope::SinceStartup,
            ));
        }
        if let Some(avg) = get("Innodb_row_lock_time_avg") {
            p.signals.push(
                Signal::new(
                    culprit_core::signal_id::ROW_LOCK_AVG,
                    culprit_core::message!("provider.mysql.signal.row_lock_avg"),
                    avg,
                    SignalUnit::Milliseconds,
                    SignalScope::SinceStartup,
                )
                .graded(&t!("provider.mysql.signal.row_lock_avg.healthy"), 50.0, 500.0, true),
            );
        }
        if let Some(ConnectionLimit { max, own, peak_ago }) = r.connections {
            // The application's connections: Culprit's own (this scan opens several) don't count.
            if let Some(used) = get("Threads_connected").map(|u| (u - own).max(0.0)) {
                p.signals.push(
                    Signal::new(
                        culprit_core::signal_id::CONNECTIONS,
                        culprit_core::message!("provider.mysql.signal.connections"),
                        used / max * 100.0,
                        SignalUnit::Percent,
                        SignalScope::Now,
                    )
                    .graded(&t!("provider.mysql.signal.connections.healthy"), 80.0, 90.0, true),
                );
            }
            if let Some(peak) = get("Max_used_connections") {
                p.signals.push(
                    Signal::new(
                        culprit_core::signal_id::MAX_USED_CONNECTIONS,
                        culprit_core::message!("provider.mysql.signal.max_used_connections"),
                        peak / max * 100.0,
                        SignalUnit::Percent,
                        SignalScope::SinceStartup,
                    )
                    .graded(
                        &t!("provider.mysql.signal.max_used_connections.healthy"),
                        90.0,
                        100.0,
                        true,
                    ),
                );
                if let Some(ago) = peak_ago {
                    p.signals.push(Signal::new(
                        culprit_core::signal_id::MAX_USED_CONNECTIONS_AGO,
                        culprit_core::message!("provider.mysql.signal.max_used_connections_ago"),
                        ago,
                        SignalUnit::Seconds,
                        SignalScope::Now,
                    ));
                }
            }
        }
    }
    // Its own signal, not the oldest transaction's: the history list is the whole server's, and it counts the undo
    // logs of committed transactions purge hasn't removed yet (each one any number of rows), not row versions.
    if let Some(h) = r.history {
        p.signals.push(
            Signal::new(
                culprit_core::signal_id::HISTORY_LENGTH,
                culprit_core::message!("provider.mysql.signal.history_length"),
                h,
                SignalUnit::Count,
                SignalScope::Now,
            )
            .graded(&t!("provider.mysql.signal.history_length.healthy"), 100_000.0, 1_000_000.0, true),
        );
    }
    if let Some(oldest) = r.oldest {
        p.signals.push(
            Signal::new(
                culprit_core::signal_id::OLDEST_TRANSACTION,
                culprit_core::message!("provider.mysql.signal.oldest_transaction"),
                oldest.seconds,
                SignalUnit::Seconds,
                SignalScope::Now,
            )
            .graded(&t!("provider.mysql.signal.oldest_transaction.healthy"), 600.0, 1800.0, true),
        );
        p.oldest_transaction = Some(oldest);
    }
    p
}

/// What each busy session is doing, from the process list: the state text and whether InnoDB has it waiting for a lock.
const SESSIONS_SQL: &str = "SELECT p.STATE AS state, t.trx_state AS trx_state
   FROM information_schema.PROCESSLIST p
   LEFT JOIN information_schema.INNODB_TRX t ON t.trx_mysql_thread_id = p.ID
  WHERE p.ID <> CONNECTION_ID() AND p.COMMAND NOT IN ('Sleep', 'Daemon', 'Binlog Dump', 'Binlog Dump GTID')
    AND p.USER <> 'system user' AND (p.INFO IS NULL OR p.INFO NOT LIKE '%/* culprit */%')";

/// The shared category for a process-list state (`Waiting for table metadata lock`, `Sending to client`, …).
pub fn state_category(state: &str, lock_wait: bool) -> &'static str {
    let s = state.to_ascii_lowercase();
    if lock_wait || s.contains("lock") {
        "Lock"
    } else if s.contains("commit") || s.contains("binlog") || s.contains("semi-sync") {
        "Log"
    } else if s.contains(" net") || s.contains("to client") {
        "Network"
    } else if s.contains("on disk") || s.contains("ondisk") || s.contains("opening tables") || s.contains("flushing") {
        "IO"
    } else if s.contains("sleep") {
        "Other"
    } else {
        "Running"
    }
}

/// Readings of performance_schema's wait totals `seconds` apart, with the process list sampled once a second in
/// between. The instruments time waits precisely but most are off by default (and all of them are without
/// performance_schema, MariaDB's default); the samples show what busy sessions were doing either way.
pub async fn sample(conn: &mut impl crate::read::QueryRead, seconds: u32) -> Result<WaitSample> {
    let started = Instant::now();
    let taken_at = query_one(conn, "SELECT DATE_FORMAT(UTC_TIMESTAMP(), '%Y-%m-%dT%H:%i:%sZ') AS now")
        .await?
        .and_then(|r| r.text("now"))
        .unwrap_or_default();
    let instruments_on =
        query_one(conn, "SELECT @@performance_schema AS ps").await.ok().flatten().is_some_and(|r| r.flag("ps"));
    let first = if instruments_on { wait_totals(conn).await.ok() } else { None };
    let status_before = status(conn).await.unwrap_or_default();
    let mut observed: BTreeMap<(&'static str, String), f64> = BTreeMap::new();
    let ticks = seconds.max(1) + 1;
    for tick in 0..ticks {
        for r in query(conn, SESSIONS_SQL).await? {
            *observed.entry(observation(r.text("state"), r.text("trx_state"))).or_default() += 1.0;
        }
        if tick + 1 < ticks {
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    }
    let second = if instruments_on { wait_totals(conn).await.ok() } else { None };
    let status_after = status(conn).await.unwrap_or_default();
    Ok(Sampled {
        taken_at,
        seconds,
        elapsed: started.elapsed().as_secs_f64().max(1.0),
        ticks,
        observed,
        first,
        second,
        status_before,
        status_after,
    }
    .summary())
}

/// One busy session's sample: its category and what it was doing, from its process-list state and whether InnoDB has
/// it waiting for a row lock.
fn observation(state: Option<String>, trx_state: Option<String>) -> (&'static str, String) {
    // Decided on the server's own words; a session without a state is executing.
    let state = state.filter(|s| !s.is_empty());
    let lock_wait = trx_state.is_some_and(|t| t.eq_ignore_ascii_case("LOCK WAIT"));
    let category = state_category(state.as_deref().unwrap_or_default(), lock_wait);
    let names_a_lock = state.as_deref().is_some_and(|s| s.to_ascii_lowercase().contains("lock"));
    let doing = state.unwrap_or_else(|| t!("provider.mysql.wait.executing"));
    let label = if lock_wait && !names_a_lock { t!("provider.mysql.wait.row_lock_wait", state = doing) } else { doing };
    (category, label)
}

/// What [`sample`] read: two readings of the wait totals and of global status, and the busy sessions it saw between.
struct Sampled {
    taken_at: String,
    seconds: u32,
    /// Seconds the sample took, measured.
    elapsed: f64,
    /// How many times the process list was read.
    ticks: u32,
    /// Busy sessions seen, by (category, what they were doing).
    observed: BTreeMap<(&'static str, String), f64>,
    first: Option<HashMap<String, (f64, f64)>>,
    second: Option<HashMap<String, (f64, f64)>>,
    status_before: HashMap<String, f64>,
    status_after: HashMap<String, f64>,
}

impl Sampled {
    fn summary(self) -> WaitSample {
        let Sampled { taken_at, seconds, elapsed, ticks, observed, first, second, status_before, status_after } = self;
        let instrumented = match (&first, &second) {
            (Some(first), Some(second)) => fold(second.iter().filter_map(|(event, (ms, n))| {
                let (ms0, n0) = first.get(event).copied().unwrap_or((0.0, 0.0));
                let delta = ms - ms0;
                (delta > 0.0).then(|| (event.clone(), delta, (n - n0).max(0.0)))
            })),
            _ => Vec::new(),
        };
        let samples = f64::from(ticks);
        let busy: f64 = observed.values().sum();
        let mut notes = vec![t!("provider.mysql.wait_sample.whole_server")];
        let span = t!("common.duration.s", n = format::digits(seconds.to_string()));
        let (categories, method) = if instrumented.iter().map(|c| c.wait_ms).sum::<f64>() > 0.0 {
            (instrumented, t!("provider.mysql.wait_sample.method.instruments", span = &span))
        } else {
            // Each observation stands for one second of one session's time.
            let mut by_category: BTreeMap<&'static str, (f64, String, f64)> = BTreeMap::new();
            for ((category, label), n) in &observed {
                let entry = by_category.entry(category).or_default();
                entry.0 += n;
                if *n > entry.2 {
                    entry.1 = label.clone();
                    entry.2 = *n;
                }
            }
            let mut sampled: Vec<WaitStat> = by_category
                .into_iter()
                .map(|(category, (n, top, _))| WaitStat {
                    category: category.into(),
                    wait_type: Some(top),
                    wait_ms: n / samples * elapsed * 1000.0,
                    tasks: Some(n),
                })
                .collect();
            sampled.sort_by(|a, b| b.wait_ms.total_cmp(&a.wait_ms));
            notes.push(match first {
                Some(_) => t!("provider.mysql.wait_sample.no_instrument_recorded"),
                None => t!("provider.mysql.wait_sample.performance_schema_off"),
            });
            (sampled, t!("provider.mysql.wait_sample.method.process_list", span = &span))
        };
        let total_ms: f64 = categories.iter().map(|c| c.wait_ms).sum();
        let delta = |k: &str| {
            let d = status_after.get(k)? - status_before.get(k)?;
            (d >= 0.0).then_some(d)
        };
        let rate = |k: &str| delta(k).map(|d| d / elapsed);
        let mut signals = Vec::new();
        if let Some(q) = rate("Questions") {
            signals.push(Signal::new(
                "questions",
                culprit_core::message!("provider.mysql.signal.statements"),
                q,
                SignalUnit::PerSecond,
                SignalScope::Sampled,
            ));
        }
        if let Some(w) = delta("Innodb_row_lock_waits") {
            signals.push(Signal::new(
                "row-lock-waits",
                culprit_core::message!("provider.mysql.signal.row_lock_waits_sampled"),
                w,
                SignalUnit::Count,
                SignalScope::Sampled,
            ));
        }
        if total_ms == 0.0 {
            notes.push(t!("provider.mysql.wait_sample.nothing_seen"));
        }
        WaitSample {
            engine: Engine::Mysql,
            taken_at,
            seconds: elapsed,
            method,
            total_ms,
            categories,
            signals,
            active_sessions: Some(busy / samples),
            notes,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn process_list_states_fold_into_categories() {
        assert_eq!(state_category("Waiting for table metadata lock", false), "Lock");
        assert_eq!(state_category("Updating", true), "Lock");
        assert_eq!(state_category("Sending to client", false), "Network");
        assert_eq!(state_category("waiting for handler commit", false), "Log");
        assert_eq!(state_category("Copying to tmp table on disk", false), "IO");
        assert_eq!(state_category("User sleep", false), "Other");
        assert_eq!(state_category("Sending data", false), "Running");
    }

    #[test]
    fn events_fold_into_categories_with_their_biggest_event() {
        assert_eq!(category("wait/io/file/innodb/innodb_log_file"), "Log");
        assert_eq!(category("wait/io/file/innodb/innodb_data_file"), "IO");
        assert_eq!(category("wait/io/table/sql/handler"), "Table IO");
        assert_eq!(category("wait/lock/metadata/sql/mdl"), "Lock");
        let folded = fold(vec![
            ("wait/io/file/innodb/innodb_data_file".into(), 30.0, 3.0),
            ("wait/io/file/sql/binlog".into(), 50.0, 1.0),
            ("wait/lock/table/sql/handler".into(), 5.0, 2.0),
        ]);
        assert_eq!(folded[0].category, "IO");
        assert_eq!(folded[0].wait_ms, 80.0);
        assert_eq!(folded[0].wait_type.as_deref(), Some("wait/io/file/sql/binlog"));
    }

    fn map(pairs: &[(&str, f64)]) -> HashMap<String, f64> {
        pairs.iter().map(|(k, v)| (k.to_string(), *v)).collect()
    }

    /// Each signal's key, label and healthy range, as a person reads them.
    fn words(signals: &[Signal]) -> Vec<(String, String, Option<String>)> {
        signals.iter().map(|s| (s.key.clone(), s.label.clone(), s.healthy.clone())).collect()
    }

    // What the pressure signals and the wait sample say, word for word: their messages come from locales/en.json.
    #[test]
    fn pressure_signals_say_what_they_measure_in_english() {
        let status = map(&[
            ("Uptime", 7_200.0),
            ("Innodb_buffer_pool_reads", 10.0),
            ("Innodb_buffer_pool_read_requests", 1_000.0),
            ("Innodb_buffer_pool_wait_free", 3.0),
            ("Created_tmp_disk_tables", 1.0),
            ("Created_tmp_tables", 10.0),
            ("Questions", 72_000.0),
            ("Innodb_row_lock_waits", 20.0),
            ("Innodb_row_lock_time", 900.0),
            ("Innodb_row_lock_time_avg", 45.0),
            ("Threads_connected", 12.0),
            ("Max_used_connections", 40.0),
        ]);
        let p = pressure_from(Readings {
            status: Some(status),
            running: Some(2.0),
            connections: Some(ConnectionLimit { max: 151.0, own: 2.0, peak_ago: Some(600.0) }),
            history: Some(1_200.0),
            oldest: Some(OpenTransaction { session: "42".into(), seconds: 90.0, ..OpenTransaction::default() }),
        });
        let healthy = |s: &str| Some(s.to_string());
        assert_eq!(
            words(&p.signals),
            [
                (
                    "buffer-pool-hit".into(),
                    "Reads served from memory".into(),
                    healthy("above 99% is healthy for most workloads")
                ),
                ("buffer-pool-reads".into(), "Pages read from disk".into(), None),
                ("buffer-pool-wait-free".into(), "Buffer pool free-page waits".into(), None),
                (
                    "tmp-disk-tables".into(),
                    "Temporary tables on disk".into(),
                    healthy("under 10% of temporary tables is healthy")
                ),
                ("questions".into(), "Statements".into(), None),
                ("threads-running".into(), "Statements running now".into(), None),
                ("row-lock-waits".into(), "Row-lock waits".into(), None),
                ("row-lock-time".into(), "Time waiting for row locks since start (whole server)".into(), None),
                ("row-lock-avg".into(), "Average row-lock wait".into(), healthy("under 50 ms is healthy")),
                ("connections".into(), "Connections in use".into(), healthy("under 80% of max_connections is healthy")),
                (
                    "max-used-connections".into(),
                    "Busiest moment".into(),
                    healthy("under 90% of max_connections is healthy")
                ),
                ("max-used-connections-ago".into(), "Since the busiest moment".into(), None),
                (
                    "history-length".into(),
                    "Undo logs not yet purged (whole server)".into(),
                    healthy(
                        "under 100K is healthy; a long history means purge is behind or a long transaction holds it"
                    )
                ),
                (
                    "oldest-transaction".into(),
                    "Oldest transaction to inspect for locks or a retained read view".into(),
                    healthy("seconds or minutes, not hours")
                ),
            ]
        );
        assert_eq!(p.oldest_transaction.map(|t| t.session), Some("42".into()));
        // Under an hour of uptime, row-lock waits are a count since start.
        let early = pressure_from(Readings {
            status: Some(map(&[("Uptime", 600.0), ("Innodb_row_lock_waits", 4.0)])),
            ..Readings::default()
        });
        assert_eq!(words(&early.signals), [("row-lock-waits".into(), "Row-lock waits since start".into(), None)]);
    }

    #[test]
    fn a_wait_sample_says_how_it_was_taken_in_english() {
        assert_eq!(observation(Some("Sending data".into()), None), ("Running", "Sending data".into()));
        assert_eq!(
            observation(Some("updating".into()), Some("LOCK WAIT".into())),
            ("Lock", "row lock wait (updating)".into())
        );
        assert_eq!(observation(None, Some("LOCK WAIT".into())), ("Lock", "row lock wait (executing)".into()));
        assert_eq!(observation(Some(String::new()), None), ("Running", "executing".into()));
        assert_eq!(
            observation(Some("Waiting for table metadata lock".into()), Some("LOCK WAIT".into())),
            ("Lock", "Waiting for table metadata lock".into())
        );
        let whole_server = "These wait and connection counters cover the whole server, including other databases. \
                            They do not establish pressure in the selected database.";
        let observed: BTreeMap<(&'static str, String), f64> =
            [(("Lock", "row lock wait (updating)".to_string()), 3.0)].into();
        let sampled = |first: Option<HashMap<String, (f64, f64)>>,
                       second: Option<HashMap<String, (f64, f64)>>,
                       observed: BTreeMap<(&'static str, String), f64>| {
            Sampled {
                taken_at: "2026-11-08T10:00:00Z".into(),
                seconds: 5,
                elapsed: 6.0,
                ticks: 6,
                observed,
                first,
                second,
                status_before: map(&[("Questions", 100.0), ("Innodb_row_lock_waits", 1.0)]),
                status_after: map(&[("Questions", 160.0), ("Innodb_row_lock_waits", 4.0)]),
            }
            .summary()
        };
        let timed = sampled(
            Some([("wait/lock/table/sql/handler".to_string(), (10.0, 1.0))].into()),
            Some([("wait/lock/table/sql/handler".to_string(), (30.0, 3.0))].into()),
            observed.clone(),
        );
        assert_eq!(
            timed.method,
            "Whole-server difference of two readings of performance_schema.events_waits_summary_global_by_event_name \
             5 s apart"
        );
        assert_eq!(timed.notes, [whole_server]);
        assert_eq!(
            words(&timed.signals),
            [
                ("questions".into(), "Statements".into(), None),
                ("row-lock-waits".into(), "Row-lock waits during sample (whole server)".into(), None),
            ]
        );
        let untimed = sampled(Some(HashMap::new()), Some(HashMap::new()), observed.clone());
        assert_eq!(
            untimed.method,
            "The whole server's process list sampled once a second for 5 s; observations are weighted over the \
             measured sample duration"
        );
        assert_eq!(
            untimed.notes,
            [
                whole_server,
                "No enabled performance_schema wait instrument recorded anything, so these come from sampling what \
                 each busy session was doing once a second."
            ]
        );
        assert_eq!(untimed.categories[0].wait_type.as_deref(), Some("row lock wait (updating)"));
        let off = sampled(None, None, BTreeMap::new());
        assert_eq!(
            off.notes,
            [
                whole_server,
                "performance_schema is off, so the server times no waits; these come from sampling what each busy \
                 session was doing once a second.",
                "No enabled wait instrument recorded anything and no session was busy. Many instruments are off by \
                 default; enable them in performance_schema.setup_instruments to see more."
            ]
        );
    }
}
