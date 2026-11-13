//! The `worker-pressure` check: SQL Server sessions waiting for a worker thread (THREADPOOL), or PostgreSQL/MySQL connections close to max_connections.

use super::*;
use crate::evidence::Requirement as Evidence;

pub fn worker_pressure_info() -> CheckInfo {
    CheckInfo {
        id: "worker-pressure".into(),
        title: t!("check.worker_pressure.info.title"),
        category: Category::Configuration,
        engines: vec![Engine::Mssql, Engine::Postgres, Engine::Mysql],
        requires: vec![],
        description: t!("check.worker_pressure.info.description"),
    }
}

check!(
    WorkerPressureCheck: worker_pressure_info,
    decide = worker_limits,
    render = worker_pressure_finding,
    case<'s> = WorkerPressure,
    evidence = &[Evidence::Pressure {
        signal: Some(culprit_core::signal_id::CONNECTIONS),
        timed_files: false,
        complete: true
    }],
);

/// The worker-pressure findings of `snapshot`: its cases, each worded.
#[cfg(test)]
pub(crate) fn worker_pressure(snapshot: &Snapshot) -> Vec<Finding> {
    culprit_core::Check::run(&WorkerPressureCheck, snapshot)
}

/// Running out of the server's workers (SQL Server) or connections (PostgreSQL, MySQL).
pub enum WorkerPressure {
    /// Requests waiting for a worker thread (THREADPOOL): queueing when the scan ran, or on record since the start.
    Threadpool {
        /// Requests in this database waiting for a worker when the scan ran.
        waiting: f64,
        /// Time waited since the start, and on average an hour.
        total: Option<f64>,
        per_hour: Option<f64>,
        /// The waits since the start are this database's to grade: an hour of counters, ten seconds an hour or more.
        history: bool,
        severity: Severity,
        confidence: Confidence,
    },
    /// The whole server's connections near max_connections now, or at it once since the start.
    Connections {
        /// Connections in use, in percent of max_connections.
        used: f64,
        peak: Option<Peak>,
        /// Seconds since the busiest moment, when the server keeps it.
        peak_ago: Option<f64>,
    },
}

/// The busiest moment since the start, said when connections aren't near the limit now.
pub enum Peak {
    /// Above today's max_connections, in percent of it: the limit was lowered since.
    AboveLimit(f64),
    /// At the limit.
    AtLimit,
}

fn worker_limits(snapshot: &Snapshot) -> Vec<WorkerPressure> {
    match snapshot.server.engine {
        Engine::Sqlite | Engine::Yugabyte => Vec::new(),
        Engine::Mssql => {
            let waiting = pressure(snapshot)
                .and_then(|p| p.signal(culprit_core::signal_id::THREADPOOL_WAITING))
                .map(|s| s.value)
                .unwrap_or(0.0);
            let hours = counter_hours(snapshot);
            // The total since the start.
            let total = signal(snapshot, culprit_core::signal_id::THREADPOOL_WAIT_MS);
            // An average since the start says how much, not when: one bad hour days ago averages the same as a
            // steady trickle. Only over at least an hour of counters.
            let per_hour = total.zip(hours).map(|(t, h)| t / h);
            let database_history = snapshot.server.on(culprit_core::Platform::AzureSqlDatabase)
                || app_queries(snapshot).any(|q| {
                    q.observed_wait_ms(culprit_core::telemetry::WaitClass::Workers)
                        .is_some_and(|wait_ms| wait_ms >= MINUTE_MS)
                });
            let history = database_history && per_hour.is_some_and(|p| p >= 10_000.0);
            if waiting <= 0.0 && !history {
                return Vec::new();
            }
            vec![WorkerPressure::Threadpool {
                waiting,
                total,
                per_hour,
                history,
                // A queue at the moment of the scan is starvation now; totals since the start describe the past (at
                // most Medium); a task or two seen once, with nothing on record, is a moment worth watching.
                severity: if waiting >= 5.0 && history {
                    Severity::High
                } else if waiting >= 5.0 || (history && per_hour.is_some_and(|p| p >= MINUTE_MS)) {
                    Severity::Medium
                } else {
                    Severity::Low
                },
                confidence: match (history, waiting > 0.0) {
                    (true, true) => Confidence::High,
                    (true, false) => Confidence::Medium,
                    _ => Confidence::Low,
                },
            }]
        }
        Engine::Postgres | Engine::Mysql => {
            let Some(used) = signal(snapshot, culprit_core::signal_id::CONNECTIONS) else { return Vec::new() };
            let peak = signal(snapshot, culprit_core::signal_id::MAX_USED_CONNECTIONS);
            let hit_limit = peak.is_some_and(|v| v >= 100.0);
            if used < 85.0 && !hit_limit {
                return Vec::new();
            }
            vec![WorkerPressure::Connections {
                used,
                peak: peak.filter(|_| used < 85.0).map(|p| if p > 100.0 { Peak::AboveLimit(p) } else { Peak::AtLimit }),
                // When the busiest moment was (MySQL 5.7.5+ keeps it); without it, only that it happened since the
                // start.
                peak_ago: signal(snapshot, culprit_core::signal_id::MAX_USED_CONNECTIONS_AGO),
            }]
        }
    }
}

fn worker_pressure_finding(snapshot: &Snapshot, case: WorkerPressure) -> Finding {
    let key = server_key(snapshot);
    match case {
        WorkerPressure::Threadpool { waiting, total, per_hour, history, severity, confidence } => {
            let mut f = finding("worker-pressure", &key, Category::Configuration, subject(SubjectKind::Database, &key));
            let count = format::count(waiting);
            f.title = if waiting > 0.0 {
                t!("check.worker_pressure.title.waiting", n = waiting, count = &count)
            } else {
                t!(
                    "check.worker_pressure.title.waited",
                    time = format::duration(total.unwrap_or(0.0)),
                    since = uptime_phrase(snapshot)
                )
            };
            let mut parts = Vec::new();
            if history {
                parts.push(t!(
                    "check.worker_pressure.summary.history",
                    since = uptime_opening(snapshot),
                    time = format::duration(total.unwrap_or(0.0)),
                    per_hour = format::duration(per_hour.unwrap_or(0.0))
                ));
            }
            if waiting > 0.0 {
                parts.push(t!("check.worker_pressure.summary.waiting", n = waiting, count = &count));
            }
            parts.push(t!("check.worker_pressure.summary.symptom"));
            f.summary = parts.join(" ");
            f.severity = severity;
            f.impact.score = base_score(f.severity);
            f.impact.set_label(t!("check.worker_pressure.impact.threadpool"));
            f.confidence = confidence;
            if !history {
                f.caveats.push(seen_once());
            }
            if let Some(t) = total.filter(|t| *t > 0.0) {
                f.metrics.push(Metric::said(culprit_core::key!("check.worker_pressure.metric.waited"), t, Unit::Ms));
            }
            if waiting > 0.0 {
                f.metrics.push(Metric::said(
                    culprit_core::key!("check.worker_pressure.metric.waiting"),
                    waiting,
                    Unit::Count,
                ));
            }
            f.fix = Some(Fix { summary: t!("check.worker_pressure.fix.threadpool"), sql: None, cost: None });
            f.verify = vec![t!("check.worker_pressure.verify.threadpool")];
            f
        }
        WorkerPressure::Connections { used, peak, peak_ago } => {
            let mut f = finding(
                "worker-pressure",
                &key,
                Category::Configuration,
                subject(SubjectKind::Setting, "max_connections"),
            );
            let engine = snapshot.server.name();
            let percent = format!("{used:.0}");
            f.title = match peak_ago {
                _ if used >= 85.0 => t!("check.worker_pressure.title.connections", percent = &percent),
                Some(s) => {
                    t!("check.worker_pressure.title.limit_ago", engine = engine, ago = format::duration(s * 1000.0))
                }
                None => t!("check.worker_pressure.title.limit_since", engine = engine, since = uptime_phrase(snapshot)),
            };
            let postgres = snapshot.server.engine == Engine::Postgres;
            let mut summary = vec![t!("check.worker_pressure.summary.connections")];
            match peak {
                Some(Peak::AboveLimit(p)) => {
                    summary.push(t!("check.worker_pressure.summary.lowered", percent = format!("{p:.0}")))
                }
                Some(Peak::AtLimit) => {
                    summary.push(t!("check.worker_pressure.summary.busiest", engine = engine, percent = &percent))
                }
                None => {}
            }
            summary.push(if postgres {
                t!("check.worker_pressure.summary.idle_postgres")
            } else {
                t!("check.worker_pressure.summary.idle_mysql")
            });
            summary.push(t!("check.worker_pressure.summary.fewer"));
            f.summary = summary.join(" ");
            // Now is measured; a peak since the start is one moment in the past. The server's capacity is relevant
            // context, but this count does not identify this database as its cause.
            f.severity = Severity::Low;
            f.impact.score = base_score(f.severity);
            f.impact.set_label(t!("check.worker_pressure.impact.connections", percent = &percent));
            f.confidence = Confidence::High;
            f.metrics = vec![Metric::said(
                culprit_core::key!("check.worker_pressure.metric.connections"),
                used / 100.0,
                Unit::Ratio,
            )];
            f.fix = Some(Fix {
                summary: if postgres {
                    t!("check.worker_pressure.fix.postgres")
                } else {
                    t!("check.worker_pressure.fix.mysql")
                },
                sql: None,
                cost: None,
            });
            f.verify = vec![t!("check.worker_pressure.verify.connections")];
            f
        }
    }
}
