//! The dashboard's storage, connections, replication and failing-queries tiles: one sentence each from the server's signals.

use super::*;

/// The worst finding of one check, for a tile that leads with it.
/// Findings arrive ranked by impact; among equally severe ones the first is the most important. (`max_by_key` keeps
/// the last of equal maximums, hence the reversal.)
pub(super) fn worst_finding<'a>(report: &'a Report, check: &str) -> Option<&'a Finding> {
    report.findings.iter().filter(|f| f.check == check && f.is_actionable()).rev().max_by_key(|f| f.severity)
}

/// A collector's note, when it explains why a tile has nothing to show.
pub(super) fn collector_note(s: &Snapshot, names: &[&str]) -> Option<String> {
    s.notes
        .iter()
        .find(|n| names.iter().any(|name| culprit_core::same_source(name, &n.collector)))
        .map(|n| n.message.clone())
}

pub(super) fn file_name(s: &Snapshot, f: &FileIo) -> String {
    match (s.server.engine, f.kind, f.database.as_deref()) {
        (Engine::Mssql, FileKind::Log, Some(db)) => t!("view.dashboard.storage.file.log", database = db),
        (Engine::Mssql, _, Some(db)) if db.eq_ignore_ascii_case("tempdb") => "tempdb".into(),
        (Engine::Postgres, FileKind::Log, _) => t!("view.dashboard.storage.file.wal"),
        (Engine::Postgres, FileKind::Temp, _) => t!("view.dashboard.storage.file.temp"),
        (Engine::Postgres, _, _) => t!("view.dashboard.storage.file.data"),
        (Engine::Mssql | Engine::Yugabyte | Engine::Mysql | Engine::Sqlite, _, _) => f.name.clone(),
    }
}

/// A latency people can read: cache-fast storage in microseconds ("3 µs", as the Health panel says), not "0.00 ms".
/// Storage latency in one unit family: microseconds below a millisecond ("40 µs", "630 µs"), so SQL Server's 0.04 ms
/// and MySQL's 8 µs read alike.
pub(super) fn latency(ms: f64) -> String {
    if ms < 1.0 {
        t!("common.duration.us", n = (ms * 1000.0).round().max(1.0).to_string())
    } else {
        format::duration(ms)
    }
}

/// What a file is waited on for: reads of data, writes or flushes of the log.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Waited {
    Read,
    Write,
    Flush,
}

/// Whether the log's writes are flushes (`fsync`) rather than writes: the engine says which it timed.
fn flushes(f: &FileIo) -> bool {
    f.write_operation == Some(culprit_core::signals::FileWriteOperation::Flush)
}

/// The latency a file is waited on for (reads of data, writes of the log) and how many operations it averages.
pub(super) fn waited_latency(f: &FileIo) -> Option<(f64, f64, Waited)> {
    if f.kind == FileKind::Log {
        let op = if flushes(f) { Waited::Flush } else { Waited::Write };
        f.avg_write_ms().map(|avg| (avg, f.writes, op))
    } else {
        f.avg_read_ms().map(|avg| (avg, f.reads, Waited::Read))
    }
}

/// Average read latency of data and temporary files, and write latency of the log: what queries and commits wait on.
pub(super) fn storage(report: &Report) -> Tile {
    let s = &report.snapshot;
    let family = family(&s.server);
    let merged = s.pressure.as_ref().map(|p| merge_logs(&p.files)).unwrap_or_default();
    let files: Vec<&FileIo> =
        merged.iter().filter(|f| f.kind != FileKind::Other && waited_latency(f).is_some()).collect();
    if files.is_empty() {
        // An engine fact only when the scan proves it; otherwise just what this scan lacks (a login that can't read
        // the views, a source that failed).
        let why = match s.server.engine {
            Engine::Postgres if is_off(setting(s, "track_io_timing")) => t!("view.dashboard.storage.postgres_untimed"),
            Engine::Mysql if !s.server.has(Capability::PerformanceSchema) => {
                t!("view.dashboard.storage.no_performance_schema", family = family)
            }
            Engine::Mssql | Engine::Postgres | Engine::Yugabyte | Engine::Mysql | Engine::Sqlite => {
                t!("view.dashboard.storage.none")
            }
        };
        let mut t = Tile::new(TileKey::Storage, TileStatus::Unavailable, why);
        t.detail = collector_note(s, &[culprit_core::source_id::FILE_LATENCY, "storage", "files", "signals"]);
        return t;
    }
    // Enough operations for an average to mean something: the floor the slow-storage check uses.
    let worst = |log: bool| {
        files
            .iter()
            .filter(|f| (f.kind == FileKind::Log) == log)
            .filter_map(|f| waited_latency(f).filter(|(_, ops, _)| *ops >= 1_000.0).map(|(avg, _, _)| avg))
            .max_by(f64::total_cmp)
    };
    let (read, log) = (worst(false), worst(true));
    // The log's operation as the engine timed it: a flush on any log file says flushes.
    let flush = files.iter().any(|f| f.kind == FileKind::Log && flushes(f));
    let op = if flush { "flush" } else { "write" };
    let (status, takeaway) = match worst_finding(report, "slow-storage") {
        Some(f) => (severity_status(f.severity), format!("{}.", f.title.trim_end_matches('.'))),
        None => match (read.map(latency), log.map(latency)) {
            (None, None) => (TileStatus::Info, t!("view.dashboard.storage.too_little")),
            (Some(read), None) => (TileStatus::Info, t!("view.dashboard.storage.averages_read", read = read)),
            (None, Some(log)) => (TileStatus::Info, t!("view.dashboard.storage.averages_log", op = op, log = log)),
            (Some(read), Some(log)) => {
                (TileStatus::Info, t!("view.dashboard.storage.averages_both", read = read, op = op, log = log))
            }
        },
    };
    let mut t = Tile::new(TileKey::Storage, status, takeaway);
    if let Some(avg) = read {
        t.metrics.push(TileMetric::new("read-ms", &t!("view.dashboard.storage.metric.read"), Some(avg), latency(avg)));
    }
    if let Some(avg) = log {
        let label = if flush {
            t!("view.dashboard.storage.metric.log_flush")
        } else {
            t!("view.dashboard.storage.metric.log_write")
        };
        t.metrics.push(TileMetric::new("log-write-ms", &label, Some(avg), latency(avg)));
    }
    // The slowest files, each by what it's waited on for.
    let mut rows: Vec<(f64, TileItem)> = files
        .iter()
        .filter_map(|f| {
            let (avg, ops, what) = waited_latency(f)?;
            (ops >= 1_000.0).then(|| {
                let id = format!("{}:{}", f.database.as_deref().unwrap_or(""), f.name);
                let name = file_name(s, f);
                let label = if f.scope == Some(culprit_core::FileScope::Server) {
                    t!("view.dashboard.storage.whole_server", file = name)
                } else if f.scope == Some(culprit_core::FileScope::Database)
                    && f.database.as_deref().is_none_or(|db| db == s.server.database)
                {
                    t!("view.dashboard.storage.this_database", file = name)
                } else {
                    name
                };
                let mut item = TileItem::new(ItemKind::Signal, id, label);
                let average = latency(avg);
                item.value = Some(match what {
                    Waited::Read => t!("view.dashboard.storage.per_read", latency = average),
                    Waited::Write => t!("view.dashboard.storage.per_write", latency = average),
                    Waited::Flush => t!("view.dashboard.storage.per_flush", latency = average),
                });
                let prefix = format!("slow-storage:{}:", f.database.as_deref().unwrap_or(""));
                item.status = Some(
                    report
                        .findings
                        .iter()
                        .filter(|g| g.is_actionable() && g.id.starts_with(&prefix))
                        // The finding names its files in its one text metric (its label is words, in the reader's
                        // language): the names, comma-separated.
                        .filter(|g| {
                            g.metrics.iter().any(|m| {
                                m.unit == culprit_core::Unit::Text
                                    && m.text
                                        .as_ref()
                                        .is_some_and(|names| names == &f.name || names.split(", ").any(|n| n == f.name))
                            })
                        })
                        .map(|g| severity_status(g.severity))
                        .max()
                        .unwrap_or(TileStatus::Info),
                );
                (avg, item)
            })
        })
        .collect();
    rows.sort_by(|a, b| b.0.total_cmp(&a.0));
    t.items = rows.into_iter().map(|(_, i)| i).take(4).collect();
    // The counters' own window, worded like every other one (a statistics reset on PostgreSQL, else the restart).
    t.detail =
        Some(files.iter().map(|f| format!("{}: {}", file_name(s, f), f.coverage(s))).collect::<Vec<_>>().join("; "));
    t
}

/// A pressure signal as a tile row: its label, value and grade.
pub(super) fn signal_item(sig: &Signal) -> TileItem {
    let mut item = TileItem::new(ItemKind::Signal, sig.key.clone(), sig.label.clone());
    item.value = Some(signal_value(sig));
    item.status = Some(signal_status(sig.status));
    item
}

/// New connections per second and connections in use: an application that connects for every request pays for it
/// on every request.
pub(super) fn connections(report: &Report) -> Tile {
    let s = &report.snapshot;
    let p = s.pressure.as_ref();
    let rate = p.and_then(|p| p.signal(culprit_core::signal_id::NEW_CONNECTIONS));
    let used = p.and_then(|p| p.signal(culprit_core::signal_id::CONNECTIONS));
    if rate.is_none() && used.is_none() {
        let why = match s.server.engine {
            Engine::Postgres if !culprit_core::version_at_least(&s.server.version, [14, 0, 0]) => {
                t!("view.dashboard.connections.postgres_old")
            }
            engine @ (Engine::Mssql | Engine::Postgres | Engine::Yugabyte | Engine::Mysql | Engine::Sqlite)
                if !engine.has_server() =>
            {
                t!("view.dashboard.connections.serverless", engine = engine.to_string())
            }
            Engine::Mssql | Engine::Postgres | Engine::Yugabyte | Engine::Mysql | Engine::Sqlite => {
                t!("view.dashboard.connections.none")
            }
        };
        let mut t = Tile::new(TileKey::Connections, TileStatus::Unavailable, why);
        t.detail = collector_note(
            s,
            &[culprit_core::source_id::CONNECTIONS, culprit_core::source_id::LOGINS, culprit_core::source_id::SESSIONS],
        );
        return t;
    }
    let (status, takeaway) = match worst_finding(report, "connection-churn") {
        Some(f) => (severity_status(f.severity), format!("{}.", f.title.trim_end_matches('.'))),
        None => match rate {
            Some(r) if r.value >= 1.0 => {
                (TileStatus::Info, t!("view.dashboard.connections.per_second", count = format::count(r.value)))
            }
            Some(r) => (
                TileStatus::Info,
                t!("view.dashboard.connections.per_minute", count = format::count((r.value * 60.0).round())),
            ),
            None => (TileStatus::Info, t!("view.dashboard.connections.no_rate")),
        },
    };
    let mut t = Tile::new(TileKey::Connections, status, takeaway);
    // Under one a second, a per-minute count reads better than "0.0".
    if let Some(r) = rate {
        t.metrics.push(if r.value >= 1.0 {
            TileMetric::new(
                "per-second",
                &t!("view.dashboard.connections.metric.per_second"),
                Some(r.value),
                format::count(r.value),
            )
        } else {
            let per_minute = (r.value * 60.0).round();
            TileMetric::new(
                "per-minute",
                &t!("view.dashboard.connections.metric.per_minute"),
                Some(per_minute),
                format::count(per_minute),
            )
        });
    }
    if let Some(u) = used {
        let item = signal_item(u);
        // Running out of connections is worse than opening too many.
        if let Some(st @ (TileStatus::Bad | TileStatus::Watch)) = item.status {
            t.status = t.status.max(st);
        }
        t.items.push(item);
    }
    for key in [
        culprit_core::signal_id::SESSION_LENGTH,
        culprit_core::signal_id::POOLED_REUSE,
        culprit_core::signal_id::THREAD_CACHE_MISSES,
        culprit_core::signal_id::SESSIONS_ABANDONED,
    ] {
        if let Some(sig) = p.and_then(|p| p.signal(key)) {
            t.items.push(signal_item(sig));
        }
    }
    t.items.truncate(4);
    t
}

pub(super) fn lag_text(r: &Replica) -> String {
    r.lag_seconds
        .map(|sec| t!("view.dashboard.replication.behind", duration = format::duration(sec * 1000.0)))
        .or_else(|| r.lag_bytes.map(|b| t!("view.dashboard.replication.log", size = format::bytes(b))))
        .or_else(|| r.state_words())
        .unwrap_or_else(|| t!("view.dashboard.replication.in_sync"))
}

/// How far replicas and replication slots are behind, worst first.
pub(super) fn replication(report: &Report) -> Tile {
    let s = &report.snapshot;
    let family = family(&s.server);
    let replicas = s.pressure.as_ref().map(|p| p.replicas.as_slice()).unwrap_or_default();
    if replicas.is_empty() {
        if let Some(note) = collector_note(s, &[culprit_core::source_id::REPLICATION]) {
            let mut t = Tile::new(
                TileKey::Replication,
                TileStatus::Unavailable,
                t!("view.dashboard.replication.unreadable", family = family),
            );
            t.detail = Some(note);
            return t;
        }
        let none = match s.server.engine {
            Engine::Yugabyte => {
                return Tile::new(
                    TileKey::Replication,
                    TileStatus::Unavailable,
                    t!("view.dashboard.replication.yugabyte"),
                );
            }
            Engine::Postgres => t!("view.dashboard.replication.none_postgres"),
            Engine::Mssql => t!("view.dashboard.replication.none_mssql"),
            Engine::Mysql => t!("view.dashboard.replication.none_mysql", family = family),
            Engine::Sqlite => t!("view.dashboard.replication.none_sqlite"),
        };
        return Tile::new(TileKey::Replication, TileStatus::Good, none);
    }
    let (status, takeaway) = match worst_finding(report, "replica-lag") {
        Some(f) => (severity_status(f.severity), format!("{}.", f.title.trim_end_matches('.'))),
        None => (
            TileStatus::Info,
            t!("view.dashboard.replication.recorded", n = replicas.len(), count = replicas.len().to_string()),
        ),
    };
    let mut t = Tile::new(TileKey::Replication, status, takeaway);
    let mut rows: Vec<&Replica> = replicas.iter().collect();
    rows.sort_by(|a, b| {
        b.lag_seconds
            .unwrap_or(0.0)
            .total_cmp(&a.lag_seconds.unwrap_or(0.0))
            .then(b.lag_bytes.unwrap_or(0.0).total_cmp(&a.lag_bytes.unwrap_or(0.0)))
    });
    t.items = rows
        .into_iter()
        .take(4)
        .map(|r| {
            let label = match r.role {
                ReplicaRole::Replica => t!("view.dashboard.replication.replica", name = &r.name),
                ReplicaRole::ThisServer => t!("view.dashboard.replication.this_server", name = &r.name),
                ReplicaRole::Slot => t!("view.dashboard.replication.slot", name = &r.name),
            };
            let mut item = TileItem::new(ItemKind::Signal, r.name.clone(), label);
            item.value = Some(lag_text(r));
            item.status = Some(
                report
                    .findings
                    .iter()
                    .filter(|f| f.check == "replica-lag" && f.is_actionable() && f.subject.name == r.name)
                    .map(|f| severity_status(f.severity))
                    .max()
                    .unwrap_or(TileStatus::Info),
            );
            item
        })
        .collect();
    t
}

/// Statements whose runs fail or are cancelled (timeouts), from the counts the engine keeps per statement.
pub(super) fn failures(report: &Report) -> Tile {
    let s = &report.snapshot;
    if let Some(history) = &s.operations.termination_history {
        let count = history.events.len();
        let mut tile = Tile::new(
            TileKey::Failures,
            TileStatus::Info,
            if count == 0 {
                t!("view.dashboard.failures.terminations_none")
            } else {
                t!("view.dashboard.failures.terminations", n = count, count = count.to_string())
            },
        );
        tile.title = t!("view.dashboard.failures.terminations_title");
        let mut detail = vec![t!(
            "view.dashboard.failures.terminations_period",
            source = &history.source,
            start = &history.requested_start,
            end = &history.requested_end,
            visibility = &history.visibility
        )];
        if history.partial {
            detail.push(t!("view.dashboard.partial_read"));
        }
        detail.push(t!("view.dashboard.failures.terminations_scope"));
        tile.detail = Some(detail.join(" "));
        tile.metrics.push(TileMetric::new(
            "records",
            &t!("view.dashboard.failures.metric.retained"),
            Some(count as f64),
            count.to_string(),
        ));
        tile.items = history
            .events
            .iter()
            .take(4)
            .enumerate()
            .map(|(i, e)| {
                let mut item = TileItem::new(ItemKind::Signal, i.to_string(), e.reason.label());
                item.value = Some(e.ended_at.clone());
                item
            })
            .collect();
        return tile;
    }
    if let Some(reason) = s.source_problem(&[culprit_core::source_id::DOCDB_TERMINATIONS]) {
        let mut tile = Tile::new(
            TileKey::Failures,
            TileStatus::Unavailable,
            t!("view.dashboard.failures.terminations_unavailable"),
        );
        tile.detail = Some(reason);
        return tile;
    }
    if !s.operations.log_failures.is_empty() {
        let total: f64 = s.operations.log_failures.iter().map(|f| f.count).sum();
        let found: Vec<_> = report.findings.iter().filter(|f| f.check == "logged-errors").collect();
        let worst = found.iter().filter(|f| f.is_actionable()).max_by_key(|f| f.severity);
        let mut tile = Tile::new(
            TileKey::Failures,
            worst.map_or(TileStatus::Info, |f| severity_status(f.severity)),
            worst.map_or_else(
                || t!("view.dashboard.failures.log_errors", count = format::count(total)),
                |f| f.title.clone(),
            ),
        );
        tile.detail = Some(t!("view.dashboard.failures.log_detail"));
        tile.metrics.push(TileMetric::new(
            "records",
            &t!("view.dashboard.failures.metric.log_records"),
            Some(total),
            format::count(total),
        ));
        tile.items = found
            .iter()
            .take(4)
            .map(|f| {
                let mut item = TileItem::new(ItemKind::Finding, f.id.clone(), f.title.clone());
                item.finding_id = Some(f.id.clone());
                item.status = Some(if f.is_actionable() { severity_status(f.severity) } else { TileStatus::Info });
                item
            })
            .collect();
        return tile;
    }
    let family = family(&s.server);
    let Some(w) = &s.workload else {
        return no_workload(TileKey::Failures, t!("view.dashboard.no_workload.failures"));
    };
    let counted: Vec<(&QueryStat, f64)> =
        app_queries(s).filter_map(|q| q.costs.as_ref()?.unfinished().map(|u| (q, u))).collect();
    if counted.is_empty() {
        let why = match s.server.engine {
            Engine::Postgres => t!("view.dashboard.failures.postgres"),
            Engine::Mssql | Engine::Yugabyte | Engine::Mysql | Engine::Sqlite
                if w.kind() == Some(culprit_core::WorkloadKind::SlowLog) =>
            {
                t!("view.dashboard.failures.slow_log", family = family)
            }
            Engine::Mssql | Engine::Yugabyte | Engine::Mysql | Engine::Sqlite if w.log.is_some() => {
                t!("view.dashboard.failures.app_log", log = w.log.as_deref().unwrap_or_default())
            }
            Engine::Mssql | Engine::Yugabyte | Engine::Mysql | Engine::Sqlite => {
                t!("view.dashboard.failures.none_reported", family = family)
            }
        };
        return Tile::new(TileKey::Failures, TileStatus::Unavailable, why);
    }
    let mut failing: Vec<(&QueryStat, f64, f64)> = counted
        .into_iter()
        .filter(|(_, u)| *u > 0.0)
        .map(|(q, u)| (q, u, if q.executions > 0.0 { (u / q.executions).min(1.0) } else { 1.0 }))
        .collect();
    failing.sort_by(|a, b| b.1.total_cmp(&a.1));
    let total: f64 = failing.iter().map(|f| f.1).sum();
    let window = window_phrase(w);
    let (status, takeaway) = match worst_finding(report, "failing-query") {
        Some(f) => (severity_status(f.severity), format!("{}.", f.title.trim_end_matches('.'))),
        None if failing.is_empty() => (TileStatus::Good, t!("view.dashboard.failures.none", window = &window)),
        None => {
            let runs = total.round() as usize;
            (
                TileStatus::Info,
                t!("view.dashboard.failures.unfinished", n = runs, count = runs.to_string(), window = &window),
            )
        }
    };
    let mut t = Tile::new(TileKey::Failures, status, takeaway);
    t.metrics.push(TileMetric::new(
        "statements",
        &t!("view.dashboard.failures.metric.statements"),
        Some(failing.len() as f64),
        failing.len().to_string(),
    ));
    t.metrics.push(TileMetric::new(
        "runs",
        &t!("view.dashboard.failures.metric.unfinished"),
        Some(total),
        format::count(total),
    ));
    t.items = failing
        .iter()
        .take(4)
        .map(|(q, unfinished, rate)| {
            let mut item = TileItem::new(ItemKind::Query, q.id.clone(), query_handle(q));
            item.query_id = Some(q.id.clone());
            // Fewer than five unfinished runs is too few to grade (a grey dot), so the value says how few.
            item.value = Some(if *unfinished < 5.0 {
                t!(
                    "view.dashboard.failures.runs_of",
                    failed = format::count(*unfinished),
                    runs = format::count(q.executions.max(*unfinished))
                )
            } else if *rate >= 0.999 {
                t!("view.dashboard.failures.every_run")
            } else {
                t!("view.dashboard.failures.of_runs", share = format::percent(*rate))
            });
            item.share = Some(*rate);
            let id = format!("failing-query:{}", q.id);
            item.finding_id = report.findings.iter().find(|f| f.id == id).map(|f| f.id.clone());
            item.status = Some(
                report
                    .findings
                    .iter()
                    .find(|f| f.id == id && f.is_actionable())
                    .map(|f| severity_status(f.severity))
                    .unwrap_or(TileStatus::Info),
            );
            item
        })
        .collect();
    t
}

pub(super) fn statistics(report: &Report) -> Tile {
    let s = &report.snapshot;
    let locked = report.locked.len();
    let stats_findings: Vec<&Finding> =
        report.findings.iter().filter(|f| STATISTICS_FINDINGS.contains(&f.id.as_str())).collect();
    let mut tile = match &s.workload {
        None => {
            let missing = s.server.missing.iter().find(|m| m.capability.is_workload_source());
            // Red when a source of query history is missing and can be turned on; an engine that keeps none (SQLite,
            // whose statements can come from the application's log) isn't failing at anything.
            let turn_on = missing.filter(|m| m.capability != Capability::QueryLog);
            let mut t = Tile::new(
                TileKey::Statistics,
                if turn_on.is_some() { TileStatus::Bad } else { TileStatus::Unavailable },
                match missing {
                    Some(m) => t!("view.dashboard.statistics.no_history_because", reason = sentence(&m.reason)),
                    None => t!("view.dashboard.statistics.no_history", family = family(&s.server)),
                },
            );
            t.detail = Some(if turn_on.is_some() {
                t!("view.dashboard.statistics.turn_on_detail")
            } else {
                t!("view.dashboard.statistics.no_history_detail")
            });
            t.fix_sql = turn_on.and_then(|m| m.fix.as_deref()).and_then(fix_statement);
            t.items = s
                .server
                .missing
                .iter()
                .filter(|m| m.capability.is_workload_source() && m.capability != Capability::QueryLog)
                .map(|m| {
                    let mut item = TileItem::new(ItemKind::Capability, m.capability.id(), m.capability.label());
                    item.value = Some(t!("view.dashboard.statistics.off"));
                    item.status = Some(TileStatus::Bad);
                    item
                })
                .collect();
            t
        }
        Some(w) => {
            let planned = app_queries(s).filter(|q| q.plan.is_some()).count();
            let window = window_phrase(w);
            let mut status = TileStatus::Good;
            let (n, count, with_plans) =
                (w.query_count as usize, (w.query_count as usize).to_string(), planned.to_string());
            let mut takeaway = if w.log.is_some() {
                // The phrase already names the log: "the 48 minutes the Rails log covers".
                t!(
                    "view.dashboard.statistics.log_covers",
                    n = n,
                    window = &window,
                    count = &count,
                    planned = &with_plans
                )
            } else {
                t!(
                    "view.dashboard.statistics.source_covers",
                    n = n,
                    source = &w.source,
                    window = &window,
                    count = &count,
                    planned = &with_plans
                )
            };
            if let (Some(used), Some(max)) = (
                setting(s, "query_store_options.current_storage_size_mb").and_then(|v| v.parse::<f64>().ok()),
                setting(s, "query_store_options.max_storage_size_mb").and_then(|v| v.parse::<f64>().ok()),
            ) && max > 0.0
                && used / max >= 0.9
                // With size-based cleanup on AUTO it trims its oldest data at 90% and never fills: that's how it runs.
                && !setting(s, "query_store_options.size_based_cleanup_mode_desc")
                    .is_some_and(|v| v.eq_ignore_ascii_case("AUTO"))
            {
                status = TileStatus::Watch;
                takeaway = t!("view.dashboard.statistics.query_store_full", share = format::percent(used / max));
            }
            // A log without timestamps covers a period nobody can say: not "thin", unknown.
            let short = if w.log.is_some() { !w.window_unknown() } else { !w.is_windowed() };
            if short && w.measurement_hours() < 1.0 {
                status = status.max(TileStatus::Watch);
                takeaway = if w.log.is_some() {
                    t!("view.dashboard.statistics.thin_log", window = &window)
                } else {
                    t!("view.dashboard.statistics.thin", source = &w.source, window = &window)
                };
            }
            if let Some(worst) = stats_findings.iter().rev().max_by_key(|f| f.severity) {
                status =
                    status.max(if worst.severity >= Severity::Medium { TileStatus::Watch } else { TileStatus::Info });
                if status >= TileStatus::Watch && worst.severity >= Severity::Medium {
                    takeaway = format!("{}.", worst.title.trim_end_matches('.'));
                }
            }
            let mut t = Tile::new(TileKey::Statistics, status, takeaway);
            // The slow query log only has statements over long_query_time: say so where its numbers are summarized.
            if w.kind() == Some(culprit_core::WorkloadKind::SlowLog) {
                t.detail = collector_note(s, &[culprit_core::source_id::WORKLOAD]);
            }
            t.metrics = vec![
                TileMetric::new(
                    "queries",
                    &t!("view.dashboard.metric.queries"),
                    Some(w.query_count),
                    format::count(w.query_count),
                ),
                TileMetric::new("plans", &t!("view.dashboard.metric.with_plans"), Some(planned as f64), with_plans),
                TileMetric::new(
                    "window-hours",
                    &t!("view.dashboard.metric.covers"),
                    Some(w.measurement_hours()),
                    span(w.measurement_hours()),
                ),
            ];
            t.items = stats_findings
                .iter()
                .map(|f| {
                    let mut item = TileItem::new(ItemKind::Finding, f.id.clone(), f.title.clone());
                    item.status = Some(severity_status(f.severity));
                    item.finding_id = Some(f.id.clone());
                    item
                })
                .collect();
            t.fix_sql = stats_findings.first().and_then(|f| f.fix.as_ref()).and_then(|f| f.sql.clone());
            t
        }
    };
    tile.metrics.push(TileMetric::new(
        "locked",
        &t!("view.dashboard.statistics.metric.locked"),
        Some(locked as f64),
        locked.to_string(),
    ));
    if locked > 0 && tile.detail.is_none() {
        let needs: Vec<String> = {
            let mut caps: Vec<String> =
                report.locked.iter().flat_map(|l| l.missing.iter().map(|c| c.label())).collect();
            caps.sort_unstable();
            caps.dedup();
            caps
        };
        tile.detail = Some(t!(
            "view.dashboard.statistics.locked",
            n = locked,
            count = locked.to_string(),
            checks = format::list(&report.locked.iter().map(|l| l.check.title.clone()).take(3).collect::<Vec<_>>()),
            capabilities = format::list(&needs)
        ));
    }
    tile
}
