//! MySQL and MariaDB configuration, `performance_schema`, buffer pool, and slow query log checks.

use super::*;

/// The sentence `check.config.slow_query_log.fix_cost.mariadb_lock_time` says, for the lock-holder advice that still
/// words itself (`crate::support`); the finding here says it from the catalog.
pub(crate) const MARIADB_TABLE_LOCK_TIME: &str = "Written to a table (TABLE in log_output), MariaDB counts the whole run of each statement it logs as that statement's lock time, so their lock time says nothing (Culprit reads it as unknown); written to a file (FILE), it is the real wait.";

/// What MySQL's and MariaDB's settings establish, one finding each.
pub enum Case<'s> {
    /// The buffer pool, smaller than half the InnoDB data, serving under 99% of reads from memory.
    BufferPool {
        pool: f64,
        data: f64,
        hit: f64,
        reads: Option<f64>,
    },
    /// The slow query log is off: whether long_query_time is already a second or less, and log_output with TABLE in
    /// it where it lacks it.
    SlowQueryLog {
        lower_already: bool,
        output: Option<String>,
    },
    /// performance_schema is off; `logged`: Culprit reads the slow query log instead.
    PerformanceSchemaOff {
        logged: bool,
    },
    StatementsDigest,
    /// The digest table is full: its size as set, the digests lost, the share of statement time in the anonymous row,
    /// and the size to set.
    DigestsLost {
        size: &'s str,
        lost: f64,
        untracked_share: f64,
        room: String,
    },
    /// Statements cut off at max_digest_length.
    DigestLength {
        truncated: usize,
    },
    /// Queries waited for a clean buffer-pool page this many times.
    BufferPoolWaitFree {
        waits: f64,
        severity: Severity,
    },
}

pub(super) fn decide(snapshot: &Snapshot) -> Vec<Case<'_>> {
    let mut out = settings(snapshot);
    out.extend(health(snapshot));
    out
}

fn settings(snapshot: &Snapshot) -> Vec<Case<'_>> {
    let mut out = Vec::new();
    out.extend(buffer_pool(snapshot));
    // With performance_schema off, its finding already leads with the slow query log.
    let ps_off = value(snapshot, &["performance_schema"]).is_some_and(|v| !is_on(v));
    if value(snapshot, &["slow_query_log"]).is_some_and(|v| !is_on(v)) && !ps_off {
        let output = value(snapshot, &["log_output"]).unwrap_or("FILE").trim().to_ascii_uppercase();
        let output = (!output.split(',').any(|o| o.trim() == "TABLE")).then(|| {
            if output.is_empty() || output == "NONE" { "TABLE".to_string() } else { format!("{output},TABLE") }
        });
        // A second is the threshold to suggest, never a raise: one already lower (0.2 s) logs more and stays.
        let lower_already =
            value(snapshot, &["long_query_time"]).and_then(|v| v.trim().parse::<f64>().ok()).is_some_and(|t| t <= 1.0);
        out.push(Case::SlowQueryLog { lower_already, output });
    }
    out
}

/// A buffer pool smaller than the data is a problem only when reads miss it: a large, mostly cold schema served
/// from memory 99.9% of the time needs nothing. Graded on the hit rate and how much it reads from disk, since the
/// server started (at least an hour of it). The server's memory isn't known, so no size is proposed.
fn buffer_pool(snapshot: &Snapshot) -> Option<Case<'_>> {
    let pool =
        value(snapshot, &["innodb_buffer_pool_size"]).and_then(|v| v.parse::<f64>().ok()).filter(|p| *p > 0.0)?;
    let data = innodb_bytes(snapshot);
    if data < 1024.0 * 1024.0 * 1024.0 || pool >= data * 0.5 {
        return None;
    }
    counter_days_for_rate(snapshot)?;
    let pressure = snapshot.pressure.as_ref()?;
    let hit = pressure.signal(culprit_core::signal_id::BUFFER_POOL_HIT).map(|s| s.value)?;
    if hit >= 99.0 {
        return None;
    }
    let reads = pressure.signal(culprit_core::signal_id::BUFFER_POOL_READS).map(|s| s.value);
    Some(Case::BufferPool { pool, data, hit, reads })
}

fn health(snapshot: &Snapshot) -> Vec<Case<'_>> {
    let mut out = Vec::new();
    if value(snapshot, &["performance_schema"]).is_some_and(|v| !is_on(v)) {
        let logged = snapshot.workload.as_ref().is_some_and(|w| w.kind() == Some(culprit_core::WorkloadKind::SlowLog));
        out.push(Case::PerformanceSchemaOff { logged });
        return out;
    }
    if value(snapshot, &["performance_schema.consumer.statements_digest"]).is_some_and(|v| v.eq_ignore_ascii_case("NO"))
    {
        out.push(Case::StatementsDigest);
    }
    let lost = number(snapshot, "performance_schema_digest_lost").unwrap_or(0.0);
    let untracked_ms = number(snapshot, "performance_schema.untracked_ms").unwrap_or(0.0);
    let total_ms = snapshot.workload.as_ref().map(|w| w.total_time_ms).unwrap_or(0.0);
    if lost > 0.0 || (total_ms > 0.0 && untracked_ms / (total_ms + untracked_ms) >= 0.05) {
        let size = value(snapshot, &["performance_schema_digests_size"]).unwrap_or("10000");
        let untracked_share =
            if total_ms + untracked_ms > 0.0 { untracked_ms / (total_ms + untracked_ms) } else { 0.0 };
        // More room than it has now (-1 is "sized automatically"): 50,000, or twice what it holds.
        let room = size.trim().parse::<f64>().ok().filter(|s| *s > 0.0).map_or(50_000.0, |s| (s * 2.0).max(50_000.0));
        out.push(Case::DigestsLost { size, lost, untracked_share, room: format!("{room:.0}") });
    }
    let truncated = snapshot.queries().iter().filter(|q| q.text.trim_end().ends_with("...")).count();
    if truncated >= 3 && number(snapshot, "max_digest_length").is_some_and(|v| v <= 1024.0) {
        out.push(Case::DigestLength { truncated });
    }
    if let Some(waits) = snapshot.pressure.as_ref().and_then(|p| p.signal("buffer-pool-wait-free")).map(|s| s.value)
        && let Some(rate) = counter_rate_per_day(snapshot, waits).filter(|r| *r >= 240.0)
    {
        let severity = if rate >= 2_400.0 || waits >= 1_000.0 { Severity::Medium } else { Severity::Low };
        out.push(Case::BufferPoolWaitFree { waits, severity });
    }
    out
}

pub(super) fn render(snapshot: &Snapshot, case: Case<'_>) -> Finding {
    let name = snapshot.server.name();
    match case {
        Case::BufferPool { pool, data, hit, reads } => buffer_pool_finding(snapshot, pool, data, hit, reads),
        Case::SlowQueryLog { lower_already, output } => {
            let mut changes = vec![("slow_query_log", "ON")];
            if !lower_already {
                changes.push(("long_query_time", "1"));
            }
            if let Some(output) = output.as_deref() {
                changes.push(("log_output", output));
            }
            let change = settings_change(&snapshot.server, &changes);
            let mut fix = vec![t!("check.config.slow_query_log.fix"), change.how];
            if !lower_already {
                fix.push(t!("check.config.slow_query_log.fix_new_connections"));
            }
            let mut f = setting_finding(
                "slow_query_log",
                Severity::Low,
                t!("check.config.slow_query_log.title"),
                t!("check.config.slow_query_log.summary"),
                Fix {
                    summary: fix.join(" "),
                    sql: change.sql,
                    // What the table costs the lock time every check reads: the reason Culprit asks for it said
                    // beside it.
                    cost: snapshot.server.is_mariadb().then(|| {
                        [
                            t!("check.config.slow_query_log.fix_cost.mariadb_lock_time"),
                            t!("check.config.slow_query_log.fix_cost.leaving_table_out"),
                        ]
                        .join(" ")
                    }),
                },
            );
            f.impact.score = 4.0;
            f
        }
        Case::PerformanceSchemaOff { logged } => {
            let (severity, summary) = if logged {
                (Severity::Medium, t!("check.config.performance_schema.summary_slow_log", server = name))
            } else {
                (Severity::High, t!("check.config.performance_schema.summary", server = name))
            };
            let restart = restart_change(snapshot, &[("performance_schema", "ON")]);
            let (fix_summary, sql) = match slow_log_change(snapshot).filter(|_| !logged) {
                Some(now) => (
                    [
                        t!("check.config.performance_schema.fix.slow_log_first"),
                        now.how,
                        t!("check.config.performance_schema.fix.at_next_restart", how = lowercase_first(&restart.how)),
                    ]
                    .join(" "),
                    match (now.sql, restart.sql) {
                        (Some(now), Some(restart)) => Some(format!(
                            "-- Now, no restart (connections opened earlier keep the old threshold):\n{now}\n\n{restart}"
                        )),
                        (now, restart) => now.or(restart),
                    },
                ),
                None => ([t!("check.config.performance_schema.fix.turn_on"), restart.how].join(" "), restart.sql),
            };
            let mut f = setting_finding(
                "performance_schema",
                severity,
                t!("check.config.performance_schema.title"),
                summary,
                Fix { summary: fix_summary, sql, cost: Some(t!("check.config.performance_schema.fix_cost")) },
            );
            f.confidence = Confidence::High;
            after_restart(&mut f);
            f
        }
        Case::StatementsDigest => setting_finding(
            "statements_digest",
            Severity::Medium,
            t!("check.config.statements_digest.title"),
            t!("check.config.statements_digest.summary"),
            Fix {
                summary: t!("check.config.statements_digest.fix"),
                sql: Some(
                    "UPDATE performance_schema.setup_consumers SET ENABLED = 'YES' WHERE NAME = 'statements_digest';"
                        .into(),
                ),
                cost: None,
            },
        ),
        Case::DigestsLost { size, lost, untracked_share, room } => {
            let change = restart_change(snapshot, &[("performance_schema_digests_size", room.as_str())]);
            let mut f = setting_finding(
                "performance_schema_digests_size",
                if untracked_share >= 0.2 { Severity::Medium } else { Severity::Low },
                t!("check.config.digests_lost.title"),
                t!(
                    "check.config.digests_lost.summary",
                    size = size,
                    lost = format::count(lost),
                    untracked = if untracked_share > 0.0 { "yes" } else { "no" },
                    share = format::percent(untracked_share)
                ),
                Fix {
                    summary: [t!("check.config.digests_lost.fix"), change.how].join(" "),
                    sql: Some(match change.sql {
                        Some(restart) => format!(
                            "{restart}\n-- or start over now:\nTRUNCATE TABLE performance_schema.events_statements_summary_by_digest;"
                        ),
                        None => {
                            "-- Start over now:\nTRUNCATE TABLE performance_schema.events_statements_summary_by_digest;"
                                .into()
                        }
                    }),
                    cost: None,
                },
            );
            f.confidence = Confidence::High;
            // Digests lost is a status counter: a restart starts it over; after a truncate alone, its growth is what
            // tells.
            f.verify = vec![t!("check.config.digests_lost.verify")];
            f.metrics.push(Metric::new(t!("check.config.digests_lost.metric.lost"), lost, Unit::Count));
            if untracked_share > 0.0 {
                f.metrics.push(Metric::new(
                    t!("check.config.digests_lost.metric.untracked"),
                    untracked_share,
                    Unit::Ratio,
                ));
            }
            f
        }
        Case::DigestLength { truncated } => {
            let change = restart_change(
                snapshot,
                &[
                    ("max_digest_length", "4096"),
                    ("performance_schema_max_digest_length", "4096"),
                    ("performance_schema_max_sql_text_length", "4096"),
                ],
            );
            let mut f = setting_finding(
                "max_digest_length",
                Severity::Low,
                t!("check.config.digest_length.title", count = truncated.to_string()),
                t!("check.config.digest_length.summary"),
                Fix {
                    summary: [t!("check.config.digest_length.fix"), change.how].join(" "),
                    sql: change.sql,
                    cost: Some(t!("check.config.digest_length.fix_cost")),
                },
            );
            f.confidence = Confidence::Medium;
            // The statements already cut off stay as they were recorded: only those recorded after the restart are
            // whole.
            f.verify = vec![t!("check.config.digest_length.verify")];
            f
        }
        Case::BufferPoolWaitFree { waits, severity } => {
            let count = format::count(waits);
            let mut f = setting_finding(
                "innodb_buffer_pool_wait_free",
                severity,
                t!("check.config.buffer_pool_wait_free.title", count = &count),
                t!("check.config.buffer_pool_wait_free.summary", count = &count, since = since_phrase(snapshot)),
                Fix { summary: t!("check.config.buffer_pool_wait_free.fix"), sql: None, cost: None },
            );
            f.confidence = Confidence::Medium;
            f.metrics.push(Metric::new(t!("check.config.buffer_pool_wait_free.metric.waits"), waits, Unit::Count));
            f.verify = vec![t!("check.config.buffer_pool_wait_free.verify")];
            f
        }
    }
}

fn buffer_pool_finding(snapshot: &Snapshot, pool: f64, data: f64, hit: f64, reads: Option<f64>) -> Finding {
    // Both counters cover all databases; neither measures this database's delays.
    let severity = Severity::Low;
    let name = snapshot.server.name();
    let mut f = setting_finding(
        "innodb_buffer_pool_size",
        severity,
        t!("check.config.buffer_pool.title", share = format::percent((100.0 - hit) / 100.0)),
        t!(
            "check.config.buffer_pool.summary",
            pool = format::bytes(pool),
            data = format::bytes(data),
            server = name,
            hit = format::percent(hit / 100.0),
            measured = if reads.is_some() { "yes" } else { "no" },
            reads = reads.map(format::count).unwrap_or_default()
        ),
        Fix {
            summary: match snapshot.server.platform() {
                Some(platform) => t!(
                    "check.config.buffer_pool.fix_managed",
                    controlled = if platform.settings_control().is_some() { "yes" } else { "no" },
                    control = platform.settings_control().unwrap_or_default(),
                    platform = platform.label(snapshot.server.engine)
                ),
                None => t!("check.config.buffer_pool.fix"),
            },
            sql: None,
            cost: Some(t!("check.config.buffer_pool.fix_cost")),
        },
    );
    // The hit ratio counts since the server started: a pool resized online still carries its misses from before.
    f.verify = vec![t!("check.config.buffer_pool.verify", hit = format::percent(hit / 100.0), server = name)];
    f.metrics.push(Metric::new(t!("check.config.buffer_pool.metric.from_memory"), hit / 100.0, Unit::Ratio));
    if let Some(r) = reads {
        f.metrics.push(Metric::new(t!("check.config.buffer_pool.metric.from_disk"), r, Unit::Count));
    }
    f.confidence = Confidence::Medium;
    f
}

fn lowercase_first(text: &str) -> String {
    let mut chars = text.chars();
    chars.next().map(|c| c.to_lowercase().chain(chars).collect()).unwrap_or_default()
}

/// The settings that have the server log slow statements to the table Culprit reads (and, on MariaDB, count index
/// reads), without a restart, worded where they are set ([`settings_change`]). `None` when it already does.
fn slow_log_change(snapshot: &Snapshot) -> Option<crate::support::SettingChange> {
    let mut changes: Vec<(&str, String)> = Vec::new();
    if value(snapshot, &["userstat"]).is_some_and(|v| !is_on(v)) {
        changes.push(("userstat", "ON".into()));
    }
    if value(snapshot, &["slow_query_log"]).is_some_and(|v| !is_on(v)) {
        changes.push(("slow_query_log", "ON".into()));
    }
    let output = value(snapshot, &["log_output"]).unwrap_or("FILE").trim().to_ascii_uppercase();
    if !output.split(',').any(|o| o.trim() == "TABLE") {
        let output =
            if output.is_empty() || output == "NONE" { "TABLE".to_string() } else { format!("{output},TABLE") };
        changes.push(("log_output", output));
    }
    if number(snapshot, "long_query_time").is_none_or(|t| t > 1.0) {
        changes.push(("long_query_time", "0.1".into()));
    }
    let changes: Vec<(&str, &str)> = changes.iter().map(|(n, v)| (*n, v.as_str())).collect();
    (!changes.is_empty()).then(|| settings_change(&snapshot.server, &changes))
}

/// Settings read only at startup: the server configuration (my.cnf) and a restart on a server you manage; the
/// platform's control and a reboot on a managed one.
fn restart_change(snapshot: &Snapshot, changes: &[(&str, &str)]) -> crate::support::SettingChange {
    let managed = settings_change(&snapshot.server, changes);
    if managed.sql.is_none() {
        return crate::support::SettingChange {
            how: [managed.how, t!("check.config.reboot_needed")].join(" "),
            sql: None,
        };
    }
    let lines: Vec<String> = changes.iter().map(|(n, v)| format!("{n} = {v}")).collect();
    crate::support::SettingChange {
        how: t!("check.config.my_cnf_and_restart"),
        sql: Some(format!("-- In the server configuration (my.cnf), then restart:\n[mysqld]\n{}", lines.join("\n"))),
    }
}

/// The findings this file makes of `snapshot`.
#[cfg(test)]
pub(super) fn check(snapshot: &Snapshot) -> Vec<Finding> {
    decide(snapshot).into_iter().map(|case| render(snapshot, case)).collect()
}

/// The buffer-pool finding, where the snapshot establishes one.
#[cfg(test)]
pub(super) fn buffer_pool_check(snapshot: &Snapshot) -> Option<Finding> {
    buffer_pool(snapshot).map(|case| render(snapshot, case))
}

/// Bytes of this database's InnoDB tables: those with a clustered primary key (InnoDB's), and those without a primary
/// key (InnoDB is the default engine). A table whose primary key isn't clustered is another engine's (MyISAM) and has
/// no pages in the buffer pool.
fn innodb_bytes(snapshot: &Snapshot) -> f64 {
    snapshot
        .tables
        .iter()
        .filter(|t| {
            !t.has_primary_key || snapshot.indexes_on(&t.schema, &t.name).any(|i| i.is_primary_key && i.is_clustered)
        })
        .filter_map(|t| t.total_bytes)
        .sum()
}
