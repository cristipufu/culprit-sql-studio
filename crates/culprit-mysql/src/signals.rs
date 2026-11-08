//! Signals beyond time and reads, for MySQL and MariaDB: errors and warnings per statement digest (and the error code
//! recent failed runs raised), file I/O latency (performance_schema), connections opened and threads created, replica
//! status, and column histograms. Lock time per statement is already part of the digest's waits. Neither engine keeps
//! redo bytes per statement or row-lock waits per table. Each read is independent and cheap; a failure becomes a note.

use std::collections::HashMap;

use culprit_core::{
    ColumnHistogram, Coverage, FileIo, FileKind, Pressure, QueryStat, Replica, ReplicaRole, Setting, Signal,
    SignalScope, SignalUnit, Snapshot, i18n, key, t,
};
use mysql_async::{Conn, Value};

use crate::connect::{Connector, Scope};
use crate::probe::Probe;
use crate::rows::{Rec, query, query_with};
use crate::sources;

const PICOS_PER_MS: f64 = 1_000_000_000.0;

/// Collect every signal into `snapshot` on a connection of its own, each source read through `Coverage::read`
/// (`crate::sources`): these add detail and lock nothing, so a failure is a note.
pub async fn collect(connector: &Connector, probe: &Probe, snapshot: &mut Snapshot) {
    let mut coverage = Coverage::default();
    let connection = connector.connect(Scope::Server).await;
    if let Err(error) = &connection {
        coverage.sources_failed(sources::SIGNAL_READS, error);
    }
    let Some(mut conn) = coverage.read(&sources::SIGNALS, connection) else {
        coverage.append_to(snapshot);
        return;
    };
    let mariadb = probe.version.to_ascii_lowercase().contains("mariadb");
    crate::operations::collect(&mut conn, probe, snapshot).await;
    if probe.performance_schema
        && let Some(workload) = snapshot.workload.as_mut()
    {
        coverage.read(&sources::QUERY_ERRORS, attach_errors(&mut conn, &probe.database, &mut workload.queries).await);
    }
    let found = histograms(&mut conn, &probe.database, &probe.version, mariadb).await;
    if let Some(found) = coverage.read(&sources::HISTOGRAMS, found) {
        snapshot.histograms = found;
    }
    attach_table_metadata(&mut conn, &probe.database, snapshot).await;
    let pressure = snapshot.pressure.get_or_insert_with(Pressure::default);
    if probe.performance_schema
        && let Some(files) = coverage.read(&sources::FILE_LATENCY, files(&mut conn).await)
    {
        pressure.files = files;
    }
    pressure.signals.extend(coverage.read(&sources::CONNECTIONS, connections(&mut conn).await).unwrap_or_default());
    // What replication and connection advice depend on: whether this server takes writes, and its thread cache.
    let variables =
        coverage.read(&sources::SERVER_VARIABLES, server_variables(&mut conn).await).unwrap_or_else(HashMap::new);
    let writable = variables.get("read_only").map(|v| !is_on(v));
    // Product/version-specific monitoring privilege; only relevant when replication is configured.
    let replicas = replica_status(&mut conn, writable, mariadb).await.map_err(|mut e| {
        e.message = needs_replica_privilege(&e.message, mariadb, &probe.version);
        e
    });
    if let Some(replicas) = coverage.read(&sources::REPLICATION, replicas) {
        pressure.replicas = replicas;
    }
    let _ = conn.disconnect().await;
    if let Some(value) = variables.get("thread_cache_size")
        && snapshot.setting("thread_cache_size").is_none()
    {
        snapshot.settings.push(Setting {
            name: "thread_cache_size".into(),
            value: value.clone(),
            default_value: None,
            scope: "server".into(),
        });
    }
    coverage.append_to(snapshot);
}

/// The server's `message` on failing to read replica status, and the privilege that reading it needs on this product
/// and version.
fn needs_replica_privilege(message: &str, mariadb: bool, version: &str) -> String {
    let permission = if mariadb && culprit_core::version_at_least(version, [10, 5, 9]) {
        "REPLICA MONITOR"
    } else if mariadb && culprit_core::version_at_least(version, [10, 5, 2]) {
        "REPLICATION SLAVE ADMIN"
    } else {
        "REPLICATION CLIENT"
    };
    [message.to_string(), t!("provider.mysql.replica.needs_privilege", privilege = permission)].join(" ")
}

// ── per statement ──────────────────────────────────────────────────────────────────────────────────

/// A fixed name for error codes a statement commonly fails with. The server's message can carry row values, so it is
/// never kept.
pub fn error_name(code: u32) -> Option<String> {
    Some(match code {
        1205 => t!("provider.mysql.error_name.lock_wait_timeout"),
        1213 => t!("provider.mysql.error_name.deadlock"),
        3024 => t!("provider.mysql.error_name.max_execution_time"),
        1969 => t!("provider.mysql.error_name.max_statement_time"),
        1317 => t!("provider.mysql.error_name.query_interrupted"),
        3572 => t!("provider.mysql.error_name.nowait"),
        1062 => t!("provider.mysql.error_name.duplicate_key"),
        1451 => t!("provider.mysql.error_name.row_referenced"),
        1452 => t!("provider.mysql.error_name.no_parent_row"),
        1048 => t!("provider.mysql.error_name.column_not_null"),
        1406 => t!("provider.mysql.error_name.data_too_long"),
        1264 | 1690 => t!("provider.mysql.error_name.out_of_range"),
        1292 | 1366 => t!("provider.mysql.error_name.incorrect_value"),
        1064 => t!("provider.mysql.error_name.syntax_error"),
        1054 => t!("provider.mysql.error_name.unknown_column"),
        1146 => t!("provider.mysql.error_name.no_such_table"),
        1114 => t!("provider.mysql.error_name.table_full"),
        1206 => t!("provider.mysql.error_name.lock_table_full"),
        _ => return None,
    })
}

/// An error as a statement's costs and a replica name it: its code first, then its name. Checks read the code at the
/// start, so it stays first in every language.
fn error_label(code: u32) -> String {
    match error_name(code) {
        Some(name) => format!("{code} {name}"),
        None => t!("provider.mysql.error_name.unnamed", code = code.to_string()),
    }
}

async fn attach_errors(conn: &mut Conn, database: &str, queries: &mut [QueryStat]) -> culprit_core::Result<()> {
    let digests: Vec<String> = queries.iter().filter_map(|q| q.query_hash.clone()).collect();
    if digests.is_empty() {
        return Ok(());
    }
    let marks = vec!["?"; digests.len()].join(", ");
    let params = || {
        let mut p = vec![Value::from(database.to_string())];
        p.extend(digests.iter().cloned().map(Value::from));
        p
    };
    let rows = query_with(
        conn,
        &format!(
            "SELECT DIGEST AS digest, SUM_ERRORS AS errors, SUM_WARNINGS AS warnings
               FROM performance_schema.events_statements_summary_by_digest
              WHERE SCHEMA_NAME = ? AND DIGEST IN ({marks})"
        ),
        params(),
    )
    .await?;
    let counts: HashMap<String, Rec> = rows.into_iter().map(|r| (r.string("digest"), r)).collect();

    // The recent-statement tables keep each failed run's error code: enough to name what failed.
    let mut codes: HashMap<String, HashMap<u32, f64>> = HashMap::new();
    for table in ["events_statements_history", "events_statements_history_long"] {
        let sql = format!(
            "SELECT DIGEST AS digest, MYSQL_ERRNO AS code, COUNT(*) AS n FROM performance_schema.{table}
              WHERE CURRENT_SCHEMA = ? AND MYSQL_ERRNO <> 0 AND DIGEST IN ({marks}) GROUP BY DIGEST, MYSQL_ERRNO"
        );
        if let Ok(rows) = query_with(conn, &sql, params()).await {
            for r in rows {
                if let Some(code) = r.number("code") {
                    *codes.entry(r.string("digest")).or_default().entry(code as u32).or_default() +=
                        r.number("n").unwrap_or(0.0);
                }
            }
        }
    }
    for q in queries.iter_mut() {
        let Some(digest) = q.query_hash.clone() else { continue };
        let Some(r) = counts.get(&digest) else { continue };
        let mut costs = q.costs.take().unwrap_or_default();
        costs.failed = r.number("errors");
        costs.warnings = r.number("warnings");
        costs.top_error = codes.get(&digest).and_then(|by_code| {
            let (code, _) = by_code.iter().max_by(|a, b| a.1.total_cmp(b.1))?;
            Some(error_label(*code))
        });
        q.costs = (!costs.is_empty()).then_some(costs);
    }
    Ok(())
}

// ── per server ─────────────────────────────────────────────────────────────────────────────────────

/// The kind of file an instrument reads and writes.
pub fn file_kind(event: &str, file: &str) -> FileKind {
    let event = event.to_ascii_lowercase();
    let file = file.to_ascii_lowercase();
    if event.contains("log_file")
        || event.contains("redo")
        || event.ends_with("/binlog")
        || event.ends_with("/relaylog")
        || file.contains("#ib_redo")
        || file.contains("ib_logfile")
    {
        FileKind::Log
    } else if event.contains("temp") || file.contains("ibtmp") || file.contains("#innodb_temp") {
        FileKind::Temp
    } else if event.contains("innodb_data_file") {
        FileKind::Data
    } else {
        FileKind::Other
    }
}

/// The last two path components: enough to recognize `shop/orders.ibd` without the server's directory layout.
pub fn short_path(path: &str) -> String {
    let parts: Vec<&str> = path.split(['/', '\\']).filter(|p| !p.is_empty() && *p != ".").collect();
    parts[parts.len().saturating_sub(2)..].join("/")
}

/// Read and write time per file since startup, for the 24 files that took the most time.
async fn files(conn: &mut Conn) -> culprit_core::Result<Vec<FileIo>> {
    let rows = query(
        conn,
        "SELECT FILE_NAME AS file_name, EVENT_NAME AS event_name, COUNT_READ AS read_count, SUM_TIMER_READ AS read_ps,
                SUM_NUMBER_OF_BYTES_READ AS read_bytes, COUNT_WRITE AS write_count, SUM_TIMER_WRITE AS write_ps,
                SUM_NUMBER_OF_BYTES_WRITE AS write_bytes
           FROM performance_schema.file_summary_by_instance
          WHERE COUNT_READ + COUNT_WRITE > 0
          ORDER BY SUM_TIMER_READ + SUM_TIMER_WRITE DESC LIMIT 24",
    )
    .await?;
    Ok(rows
        .iter()
        .map(|r| {
            let path = r.string("file_name");
            let reads = r.number("read_count").unwrap_or(0.0);
            let writes = r.number("write_count").unwrap_or(0.0);
            // A zero timer with operations means the instrument isn't timed, not that the disk is instant.
            let timed = |ps: Option<f64>, n: f64| ps.filter(|p| *p > 0.0 || n == 0.0).map(|p| p / PICOS_PER_MS);
            FileIo {
                name: short_path(&path),
                kind: file_kind(&r.string("event_name"), &path),
                database: None,
                reads,
                read_ms: timed(r.number("read_ps"), reads),
                read_bytes: r.number("read_bytes"),
                writes,
                write_ms: timed(r.number("write_ps"), writes),
                write_bytes: r.number("write_bytes"),
                // performance_schema's file I/O is the whole server's.
                scope: Some(culprit_core::FileScope::Server),
                stats_reset: None,
                write_operation: None,
            }
        })
        .collect())
}

/// Connections opened per second since startup, how many needed a new thread, and failed connection attempts.
async fn connections(conn: &mut Conn) -> culprit_core::Result<Vec<Signal>> {
    let rows = query(
        conn,
        "SHOW GLOBAL STATUS WHERE Variable_name IN
           ('Connections', 'Threads_created', 'Aborted_connects', 'Aborted_clients', 'Uptime')",
    )
    .await?;
    let status: HashMap<String, f64> = rows
        .iter()
        .filter_map(|r| Some((r.string("variable_name"), r.text("value")?.trim().parse::<f64>().ok()?)))
        .collect();
    Ok(connection_signals(&status))
}

/// The connection signals from SHOW GLOBAL STATUS. `Connections` counts every attempt, the ones that failed
/// (Aborted_connects: a wrong password, a timeout, a scanner) included; a pool only cuts the ones that log in.
fn connection_signals(status: &HashMap<String, f64>) -> Vec<Signal> {
    let get = |k: &str| status.get(k).copied();
    let Some(uptime) = get("Uptime").filter(|u| *u > 0.0) else { return Vec::new() };
    let mut out = Vec::new();
    if let Some(attempts) = get("Connections") {
        let connections = (attempts - get("Aborted_connects").unwrap_or(0.0)).max(0.0);
        let rate = connections / uptime;
        out.push(
            Signal::new(
                culprit_core::signal_id::NEW_CONNECTIONS,
                &t!("provider.mysql.signal.new_connections"),
                rate,
                SignalUnit::PerSecond,
                SignalScope::SinceStartup,
            )
            .graded(&t!("provider.mysql.signal.new_connections.healthy"), 5.0, 25.0, true),
        );
        if let Some(created) = get("Threads_created")
            && connections >= 1_000.0
        {
            out.push(
                Signal::new(
                    culprit_core::signal_id::THREAD_CACHE_MISSES,
                    &t!("provider.mysql.signal.thread_cache_misses"),
                    created / connections * 100.0,
                    SignalUnit::Percent,
                    SignalScope::SinceStartup,
                )
                .graded(&t!("provider.mysql.signal.thread_cache_misses.healthy"), 10.0, 50.0, true),
            );
        }
    }
    let hours = uptime / 3600.0;
    for (key, name, per_hour, since_start) in [
        (
            "aborted-connects",
            "Aborted_connects",
            key!("provider.mysql.signal.aborted_connects"),
            key!("provider.mysql.signal.aborted_connects_since_start"),
        ),
        (
            "aborted-clients",
            "Aborted_clients",
            key!("provider.mysql.signal.aborted_clients"),
            key!("provider.mysql.signal.aborted_clients_since_start"),
        ),
    ] {
        if let Some(n) = get(name) {
            // Under an hour of uptime, a rate per hour would be an extrapolation: report the count.
            let signal = if hours >= 1.0 {
                Signal::new(key, &i18n::t(per_hour, &[]), n / hours, SignalUnit::PerHour, SignalScope::SinceStartup)
            } else {
                Signal::new(key, &i18n::t(since_start, &[]), n, SignalUnit::Count, SignalScope::SinceStartup)
            };
            out.push(signal);
        }
    }
    out
}

/// Whether a server variable's value means on.
fn is_on(value: &str) -> bool {
    matches!(value.trim().to_ascii_uppercase().as_str(), "ON" | "1" | "TRUE" | "YES")
}

/// The server variables advice depends on: read_only (does it take writes) and thread_cache_size.
async fn server_variables(conn: &mut Conn) -> culprit_core::Result<HashMap<String, String>> {
    let rows = query(conn, "SHOW GLOBAL VARIABLES WHERE Variable_name IN ('read_only', 'thread_cache_size')").await?;
    Ok(rows.iter().filter_map(|r| Some((r.string("variable_name").to_ascii_lowercase(), r.text("value")?))).collect())
}

/// This server's replication channels when it is a replica: lag beyond any configured delay, thread state and the
/// last error code. `writable` is whether it takes writes (read_only off), when known.
async fn replica_status(conn: &mut Conn, writable: Option<bool>, mariadb: bool) -> culprit_core::Result<Vec<Replica>> {
    let rows = match query(conn, if mariadb { "SHOW ALL REPLICAS STATUS" } else { "SHOW REPLICA STATUS" }).await {
        Ok(rows) => rows,
        // Before MySQL 8.0.22 and MariaDB 10.5.1.
        Err(_) => query(conn, if mariadb { "SHOW ALL SLAVES STATUS" } else { "SHOW SLAVE STATUS" }).await?,
    };
    Ok(rows.iter().filter_map(|r| replica(ChannelRow::from(r), writable)).collect())
}

/// One row of SHOW REPLICA STATUS, as far as replica-lag reads it.
#[derive(Default)]
struct ChannelRow {
    channel: Option<String>,
    source: String,
    io: String,
    sql: String,
    io_errno: Option<f64>,
    sql_errno: Option<f64>,
    behind: Option<f64>,
    /// SQL_Delay: seconds the replica waits on purpose before applying (CHANGE REPLICATION SOURCE TO SOURCE_DELAY).
    delay: Option<f64>,
}

impl From<&Rec> for ChannelRow {
    fn from(r: &Rec) -> ChannelRow {
        let either = |a: &str, b: &str| r.text(a).or_else(|| r.text(b));
        ChannelRow {
            channel: either("channel_name", "connection_name").filter(|c| !c.is_empty()),
            source: either("source_host", "master_host").unwrap_or_default(),
            io: either("replica_io_running", "slave_io_running").unwrap_or_default(),
            sql: either("replica_sql_running", "slave_sql_running").unwrap_or_default(),
            io_errno: r.number("last_io_errno").filter(|n| *n > 0.0),
            sql_errno: r.number("last_sql_errno").filter(|n| *n > 0.0),
            behind: either("seconds_behind_source", "seconds_behind_master").and_then(|s| s.trim().parse().ok()),
            delay: r.number("sql_delay").filter(|d| *d > 0.0),
        }
    }
}

/// A channel as replica-lag reads it. Group Replication's own channels (group_replication_applier, …_recovery) are
/// left out: they sit idle by design and the group reports its members elsewhere. Both threads off with no error is
/// someone's STOP REPLICA, not a failure; on a server that takes writes it is usually a former replica promoted to
/// primary, with its old configuration left behind.
fn replica(r: ChannelRow, writable: Option<bool>) -> Option<Replica> {
    if r.channel.as_deref().is_some_and(|c| c.to_ascii_lowercase().starts_with("group_replication_")) {
        return None;
    }
    let error = r.sql_errno.or(r.io_errno);
    // The state and the name stay one string in every language: replica-lag decides on the state's words, and the
    // name is its finding's id.
    let state = match (r.io.eq_ignore_ascii_case("yes"), r.sql.eq_ignore_ascii_case("yes")) {
        (true, true) => "replicating".to_string(),
        (false, true) => format!("receiving stopped (I/O thread: {})", r.io),
        (true, false) => "applying stopped (SQL thread)".to_string(),
        (false, false) if error.is_some() => "stopped".to_string(),
        (false, false) if writable == Some(true) => "configured, not running (no error; accepts writes)".to_string(),
        (false, false) => "stopped by hand (no error)".to_string(),
    };
    Some(Replica {
        state: Some(state),
        // Seconds_Behind_Source counts the configured delay too: only what is beyond it is lag.
        lag_seconds: r.behind.map(|s| (s - r.delay.unwrap_or(0.0)).max(0.0)),
        delay_seconds: r.delay,
        error: error.map(|n| error_label(n as u32)),
        ..Replica::new(
            match r.channel {
                Some(c) => format!("{c} (from {})", r.source),
                None => format!("from {}", r.source),
            },
            ReplicaRole::ThisServer,
        )
    })
}

/// Column histograms in this database: MySQL 8's COLUMN_STATISTICS, MariaDB's mysql.column_stats. `None` on MySQL 5.7,
/// which has none. On MariaDB every column with statistics counts, histogram or not (`kind` None): its minimum, maximum
/// and average frequency already keep the optimizer from assuming a condition keeps every row (before 10.4,
/// histogram_size = 0 collects them without a histogram).
async fn histograms(
    conn: &mut Conn,
    database: &str,
    version: &str,
    mariadb: bool,
) -> culprit_core::Result<Option<Vec<ColumnHistogram>>> {
    let params = vec![Value::from(database.to_string())];
    if mariadb {
        let rows = query_with(
            conn,
            "SELECT db_name AS schema_name, table_name, column_name, hist_type AS kind
               FROM mysql.column_stats WHERE db_name = ?",
            params,
        )
        .await?;
        return Ok(Some(
            rows.iter()
                .map(|r| ColumnHistogram {
                    schema: r.string("schema_name"),
                    table: r.string("table_name"),
                    column: r.string("column_name"),
                    kind: r.text("kind"),
                    buckets: None,
                })
                .collect(),
        ));
    }
    let major: u32 = version.split('.').next().and_then(|m| m.parse().ok()).unwrap_or(0);
    if major < 8 {
        return Ok(None);
    }
    let rows = query_with(
        conn,
        "SELECT SCHEMA_NAME AS schema_name, TABLE_NAME AS table_name, COLUMN_NAME AS column_name,
                JSON_UNQUOTE(JSON_EXTRACT(HISTOGRAM, '$.\"histogram-type\"')) AS kind,
                JSON_LENGTH(JSON_EXTRACT(HISTOGRAM, '$.buckets')) AS buckets
           FROM information_schema.COLUMN_STATISTICS WHERE SCHEMA_NAME = ?",
        params,
    )
    .await?;
    Ok(Some(
        rows.iter()
            .map(|r| ColumnHistogram {
                schema: r.string("schema_name"),
                table: r.string("table_name"),
                column: r.string("column_name"),
                kind: r.text("kind"),
                buckets: r.number("buckets"),
            })
            .collect(),
    ))
}

async fn attach_table_metadata(conn: &mut Conn, database: &str, snapshot: &mut Snapshot) {
    if let Ok(rows) = query_with(
        conn,
        "SELECT EVENT_OBJECT_TABLE AS table_name, TRIGGER_NAME AS trigger_name, EVENT_MANIPULATION AS event_name
           FROM information_schema.TRIGGERS
          WHERE EVENT_OBJECT_SCHEMA = ?
          ORDER BY EVENT_OBJECT_TABLE, TRIGGER_NAME",
        vec![Value::from(database.to_string())],
    )
    .await
    {
        for r in rows {
            let (table, name, ev) = (r.string("table_name"), r.string("trigger_name"), r.string("event_name"));
            let code = match ev.to_ascii_uppercase().as_str() {
                "INSERT" => "I",
                "UPDATE" => "U",
                "DELETE" => "D",
                _ => "",
            };
            if !name.is_empty()
                && let Some(t) = snapshot.tables.iter_mut().find(|t| t.name.eq_ignore_ascii_case(&table))
            {
                t.storage
                    .get_or_insert_with(culprit_core::TableStorage::default)
                    .options
                    .push(format!("trigger={name}:{code}"));
            }
        }
    }
    if let Ok(rows) = query_with(
        conn,
        "SELECT TABLE_NAME AS table_name, PARTITION_NAME AS partition_name,
                PARTITION_METHOD AS method, PARTITION_DESCRIPTION AS bound
           FROM information_schema.PARTITIONS
          WHERE TABLE_SCHEMA = ? AND PARTITION_NAME IS NOT NULL
          ORDER BY TABLE_NAME, PARTITION_ORDINAL_POSITION",
        vec![Value::from(database.to_string())],
    )
    .await
    {
        for r in rows {
            let (table, part, method, bound) =
                (r.string("table_name"), r.string("partition_name"), r.string("method"), r.string("bound"));
            if method.to_ascii_uppercase().starts_with("RANGE")
                && !part.is_empty()
                && !bound.is_empty()
                && let Some(t) = snapshot.tables.iter_mut().find(|t| t.name.eq_ignore_ascii_case(&table))
            {
                t.storage
                    .get_or_insert_with(culprit_core::TableStorage::default)
                    .options
                    .push(format!("partition_bound=VALUES LESS THAN ({bound}):{part}"));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn files_are_sorted_by_kind() {
        assert_eq!(file_kind("wait/io/file/innodb/innodb_data_file", "/var/lib/mysql/shop/orders.ibd"), FileKind::Data);
        assert_eq!(
            file_kind("wait/io/file/innodb/innodb_log_file", "/var/lib/mysql/#innodb_redo/#ib_redo9"),
            FileKind::Log
        );
        assert_eq!(
            file_kind("wait/io/file/innodb/innodb_temp_file", "/var/lib/mysql/#innodb_temp/temp_1.ibt"),
            FileKind::Temp
        );
        assert_eq!(file_kind("wait/io/file/innodb/innodb_data_file", "/var/lib/mysql/ibtmp1"), FileKind::Temp);
        assert_eq!(file_kind("wait/io/file/sql/binlog", "/var/lib/mysql/binlog.000002"), FileKind::Log);
        assert_eq!(file_kind("wait/io/file/aria/MAD", "/var/lib/mysql/mysql/db.MAD"), FileKind::Other);
    }

    #[test]
    fn paths_keep_the_last_two_parts() {
        assert_eq!(short_path("/var/lib/mysql/shop/orders.ibd"), "shop/orders.ibd");
        assert_eq!(short_path("./ibdata1"), "ibdata1");
        assert_eq!(short_path("C:\\data\\shop\\orders.ibd"), "shop/orders.ibd");
    }

    fn channel(io: &str, sql: &str) -> ChannelRow {
        ChannelRow { source: "10.0.0.5".into(), io: io.into(), sql: sql.into(), ..Default::default() }
    }

    // SOURCE_DELAY = 3600 keeps the replica an hour behind on purpose: only what is beyond it is lag.
    #[test]
    fn a_delayed_replica_counts_only_the_lag_beyond_its_delay() {
        let r =
            replica(ChannelRow { behind: Some(3_630.0), delay: Some(3_600.0), ..channel("Yes", "Yes") }, None).unwrap();
        assert_eq!((r.lag_seconds, r.delay_seconds), (Some(30.0), Some(3_600.0)));
    }

    // Both threads off with no error is a STOP REPLICA; on a server that takes writes, a promoted primary's leftover.
    #[test]
    fn a_channel_stopped_by_hand_is_not_a_failure() {
        let promoted = replica(channel("No", "No"), Some(true)).unwrap();
        assert_eq!(promoted.state.as_deref(), Some("configured, not running (no error; accepts writes)"));
        assert_eq!(
            replica(channel("No", "No"), Some(false)).unwrap().state.as_deref(),
            Some("stopped by hand (no error)")
        );
        let failed = replica(ChannelRow { sql_errno: Some(1062.0), ..channel("No", "No") }, Some(true)).unwrap();
        assert_eq!((failed.state.as_deref(), failed.error.as_deref()), (Some("stopped"), Some("1062 duplicate key")));
        // Group Replication's own channels idle by design.
        let gr = ChannelRow { channel: Some("group_replication_recovery".into()), ..channel("No", "No") };
        assert!(replica(gr, Some(true)).is_none());
    }

    // Connections counts failed attempts too; the rate a pool would cut is the successful ones.
    #[test]
    fn failed_connection_attempts_are_not_new_connections() {
        let status: HashMap<String, f64> = [
            ("Uptime", 1_000.0),
            ("Connections", 30_000.0),
            ("Aborted_connects", 20_000.0),
            ("Threads_created", 5_000.0),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();
        let signals = connection_signals(&status);
        let rate = signals.iter().find(|s| s.key == "new-connections").unwrap();
        assert_eq!(rate.value, 10.0);
        assert_eq!(signals.iter().find(|s| s.key == "thread-cache-misses").unwrap().value, 50.0);
    }

    #[test]
    fn error_codes_have_fixed_names() {
        assert_eq!(error_name(1205).as_deref(), Some("lock wait timeout"));
        assert_eq!(error_name(1969).as_deref(), Some("statement timeout (max_statement_time)"));
        assert_eq!(error_name(4242), None);
    }

    // What the connection signals, the errors' names and a replica's status say, word for word: their messages come
    // from locales/en.json.
    #[test]
    fn signals_and_errors_say_what_they_are_in_english() {
        let status = |uptime: f64| -> HashMap<String, f64> {
            [
                ("Uptime", uptime),
                ("Connections", 30_000.0),
                ("Aborted_connects", 20.0),
                ("Aborted_clients", 5.0),
                ("Threads_created", 5_000.0),
            ]
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect()
        };
        let words = |signals: Vec<Signal>| -> Vec<(String, String, Option<String>)> {
            signals.into_iter().map(|s| (s.key, s.label, s.healthy)).collect()
        };
        assert_eq!(
            words(connection_signals(&status(7_200.0))),
            [
                (
                    "new-connections".into(),
                    "New connections".into(),
                    Some("under 5 a second: pooled connections are opened once".into())
                ),
                (
                    "thread-cache-misses".into(),
                    "Connections that needed a new thread".into(),
                    Some("under 10%: thread_cache_size covers new connections".into())
                ),
                ("aborted-connects".into(), "Failed connection attempts".into(), None),
                ("aborted-clients".into(), "Connections dropped without closing".into(), None),
            ]
        );
        let early = words(connection_signals(&status(600.0)));
        assert_eq!(
            early[2..],
            [
                ("aborted-connects".into(), "Failed connection attempts since start".into(), None),
                ("aborted-clients".into(), "Connections dropped without closing since start".into(), None),
            ]
        );
        let names: Vec<(u32, String)> =
            [1205, 1213, 3024, 1969, 1317, 3572, 1062, 1451, 1452, 1048, 1406, 1264, 1690, 1292, 1366, 1064, 1054]
                .into_iter()
                .chain([1146, 1114, 1206, 4242])
                .map(|code| (code, error_label(code)))
                .collect();
        assert_eq!(
            names,
            [
                (1205, "1205 lock wait timeout".to_string()),
                (1213, "1213 deadlock".into()),
                (3024, "3024 statement timeout (max_execution_time)".into()),
                (1969, "1969 statement timeout (max_statement_time)".into()),
                (1317, "1317 query interrupted".into()),
                (3572, "3572 lock not available (NOWAIT)".into()),
                (1062, "1062 duplicate key".into()),
                (1451, "1451 row is referenced by a foreign key".into()),
                (1452, "1452 foreign key has no parent row".into()),
                (1048, "1048 column cannot be null".into()),
                (1406, "1406 data too long".into()),
                (1264, "1264 value out of range".into()),
                (1690, "1690 value out of range".into()),
                (1292, "1292 incorrect value".into()),
                (1366, "1366 incorrect value".into()),
                (1064, "1064 syntax error".into()),
                (1054, "1054 unknown column".into()),
                (1146, "1146 table doesn't exist".into()),
                (1114, "1114 table is full".into()),
                (1206, "1206 lock table full".into()),
                (4242, "error 4242".into()),
            ]
        );
        assert_eq!(
            replica(ChannelRow { io_errno: Some(2003.0), ..channel("No", "Yes") }, None).unwrap().error.as_deref(),
            Some("error 2003")
        );
        assert_eq!(
            needs_replica_privilege("Access denied.", true, "10.11.6-MariaDB"),
            "Access denied. Reading replica status needs the REPLICA MONITOR privilege; it only matters if this server \
             is a replica."
        );
        assert_eq!(
            needs_replica_privilege("Access denied.", true, "10.5.4-MariaDB"),
            "Access denied. Reading replica status needs the REPLICATION SLAVE ADMIN privilege; it only matters if \
             this server is a replica."
        );
        assert_eq!(
            needs_replica_privilege("Access denied.", false, "8.4.3"),
            "Access denied. Reading replica status needs the REPLICATION CLIENT privilege; it only matters if this \
             server is a replica."
        );
    }
}
