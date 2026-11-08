//! The `failing-query` check: Statements whose runs end in an error or are cancelled by the client (usually a command timeout): Query Store's aborted and exception runs, performance_schema's errors per digest. PostgreSQL keeps no per-statement error count.

use super::*;
use crate::evidence::{QueryMeasure, Requirement as Evidence};
use culprit_core::QueryCosts;

pub fn failing_query_info() -> CheckInfo {
    CheckInfo {
        id: "failing-query".into(),
        title: t!("check.failing_query.info.title"),
        category: Category::Queries,
        engines: vec![Engine::Mssql, Engine::Mysql],
        requires: vec![],
        description: t!("check.failing_query.info.description"),
    }
}

check!(
    FailingQueryCheck: failing_query_info,
    decide = failing_runs,
    render = failing_query_finding,
    case<'s> = FailingRuns<'s>,
    evidence = &[Evidence::Query(QueryMeasure::Failures)],
);

/// A client command timeout: cancelled runs that last about as long as a common default (30 s, 60 s…).
pub(super) fn looks_like_timeout(mean_ms: f64) -> Option<f64> {
    [15.0, 30.0, 60.0, 120.0].into_iter().find(|s| (mean_ms / 1000.0 - s).abs() <= s * 0.1)
}

/// Error 3572 is FOR UPDATE NOWAIT finding the row taken: the statement asked not to wait, and the application expects
/// the error (a try-lock), so it isn't a run that failed.
pub(super) fn deliberate_error(costs: &QueryCosts) -> bool {
    costs.top_error.as_deref().is_some_and(|e| e.starts_with("3572 "))
}

/// A statement five or more of whose runs, and 1% or more, failed or were cancelled.
pub struct FailingRuns<'s> {
    q: &'s QueryStat,
    costs: &'s QueryCosts,
    unfinished: f64,
    /// Unfinished runs against all its runs.
    rate: f64,
    aborted: f64,
    failed: f64,
    /// The common command timeout its cancelled runs last about as long as, in seconds.
    timeout: Option<f64>,
    /// A failure is among the recent statements: always on SQL Server; on MySQL and MariaDB when performance_schema
    /// names a recent error.
    recent: bool,
    severity: Severity,
    fix: FailingFix,
}

/// What the fix for failing runs is.
#[derive(Clone, Copy)]
pub enum FailingFix {
    /// SQL Server runs cancelled by the client's command timeout.
    Timeout,
    /// SQL Server errors, which Query Store counts but doesn't keep: capture them with an event session.
    CaptureErrors,
    /// MySQL and MariaDB: by the recent error's code.
    Error(ErrorCode),
}

/// The MySQL error the recent failed runs raised, as the fix reads it.
#[derive(Clone, Copy)]
pub enum ErrorCode {
    /// 1205: a lock wait timed out.
    LockWaitTimeout,
    /// 1213: chosen as a deadlock's victim.
    Deadlock,
    /// 3024 (MySQL) or 1969 (MariaDB): the statement time limit.
    StatementTimeout,
    /// 1062: a duplicate key.
    Duplicate,
    Other,
}

fn failing_runs(snapshot: &Snapshot) -> Vec<FailingRuns<'_>> {
    let mssql = snapshot.server.engine == Engine::Mssql;
    let mut cases: Vec<(FailingRuns<'_>, f64)> = app_queries(snapshot)
        .filter_map(|q| {
            let costs = q.costs.as_ref()?;
            if deliberate_error(costs) {
                return None;
            }
            let unfinished = costs.unfinished()?;
            let rate = per_run(unfinished, q.executions);
            (unfinished >= 5.0 && rate >= 0.01).then_some((q, costs, unfinished, rate))
        })
        .map(|(q, costs, unfinished, rate)| {
            let aborted = costs.aborted.unwrap_or(0.0);
            let failed = costs.failed.unwrap_or(0.0);
            let timeout = costs.aborted_mean_ms.and_then(looks_like_timeout).filter(|_| aborted >= failed);
            // MySQL and MariaDB count errors per digest from when it was first seen, with no time for each: an old
            // storm keeps the share up long after it ended. Only a failed run among the recent statements
            // performance_schema keeps (the error named above) shows it still happens.
            let recent = mssql || costs.top_error.is_some();
            // High needs it to keep happening, not one incident: ten unfinished runs a day (one blocking episode that
            // timed out five runs of a week's forty is a tenth of them, and Medium).
            let often = window_at_least(snapshot, q, unfinished, 10.0);
            let severity = if !recent {
                Severity::Low
            } else if rate >= 0.10 && often {
                Severity::High
            } else if rate >= 0.02 {
                Severity::Medium
            } else {
                Severity::Low
            };
            let timeouts = timeout.is_some() || aborted > failed;
            let fix = match snapshot.server.engine {
                Engine::Mssql if timeouts => FailingFix::Timeout,
                Engine::Mssql => FailingFix::CaptureErrors,
                Engine::Mysql | Engine::Postgres | Engine::Yugabyte | Engine::Sqlite => {
                    FailingFix::Error(match costs.top_error.as_deref().unwrap_or("").split_whitespace().next() {
                        Some("1205") => ErrorCode::LockWaitTimeout,
                        Some("1213") => ErrorCode::Deadlock,
                        Some("3024" | "1969") => ErrorCode::StatementTimeout,
                        Some("1062") => ErrorCode::Duplicate,
                        _ => ErrorCode::Other,
                    })
                }
            };
            let case = FailingRuns { q, costs, unfinished, rate, aborted, failed, timeout, recent, severity, fix };
            (case, query_score(snapshot, q, severity))
        })
        .collect();
    cases.sort_by(|a, b| b.1.total_cmp(&a.1));
    cases.truncate(5);
    cases.into_iter().map(|(case, _)| case).collect()
}

fn failing_query_finding(snapshot: &Snapshot, runs: FailingRuns<'_>) -> Finding {
    let FailingRuns { q, costs, unfinished, rate, aborted, failed, timeout, .. } = runs;
    let mssql = snapshot.server.engine == Engine::Mssql;
    let mut f = finding("failing-query", &q.id, Category::Queries, subject(SubjectKind::Query, &q.id));
    let handle = capitalize(&query_handle(q));
    // How often, as the title's select reads it: on every run, or a share of the time.
    let (how_often, share) = (if rate >= 0.999 { "every" } else { "share" }, format::percent(rate));
    // A statement that stopped running is told in the past.
    let tense = tense(snapshot, &[q]);
    f.title = match (aborted > 0.0, failed > 0.0) {
        (true, true) => t!(
            "check.failing_query.title.fails_or_times_out",
            handle = &handle,
            tense = tense,
            how_often = how_often,
            share = &share
        ),
        (true, false) => match timeout {
            Some(s) => t!(
                "check.failing_query.title.times_out",
                handle = &handle,
                tense = tense,
                how_often = how_often,
                share = &share,
                seconds = format!("{s:.0}")
            ),
            None => t!(
                "check.failing_query.title.cancelled",
                handle = &handle,
                tense = tense,
                how_often = how_often,
                share = &share
            ),
        },
        _ => match &costs.top_error {
            Some(e) => t!(
                "check.failing_query.title.fails_with",
                handle = &handle,
                tense = tense,
                how_often = how_often,
                share = &share,
                error = e
            ),
            None => t!(
                "check.failing_query.title.fails",
                handle = &handle,
                tense = tense,
                how_often = how_often,
                share = &share
            ),
        },
    };
    let (runs_all, ran) = (format::count(q.executions), ran_phrase(snapshot, q));
    let mut parts = vec![if unfinished >= q.executions {
        t!("check.failing_query.summary.none_finished", runs = runs_all, ran = ran)
    } else {
        t!("check.failing_query.summary.unfinished", unfinished = format::count(unfinished), runs = runs_all, ran = ran)
    }];
    if aborted > 0.0 {
        let cancelled = format::count(aborted);
        parts.push(match (costs.aborted_mean_ms, timeout) {
            (Some(ms), Some(_)) => {
                t!("check.failing_query.summary.cancelled_timeout", cancelled = cancelled, after = format::duration(ms))
            }
            (Some(ms), None) => {
                t!("check.failing_query.summary.cancelled_after", cancelled = cancelled, after = format::duration(ms))
            }
            (None, _) => t!("check.failing_query.summary.cancelled", cancelled = cancelled),
        });
    }
    if failed > 0.0 {
        parts.push(match &costs.top_error {
            Some(e) => t!("check.failing_query.summary.errors_recent", failed = format::count(failed), error = e),
            None => t!("check.failing_query.summary.errors", failed = format::count(failed)),
        });
    }
    parts.push(t!("check.failing_query.summary.cost"));
    f.summary = parts.join(" ");
    f.severity = runs.severity;
    with_query(snapshot, &mut f, q);
    severity_score(&mut f);
    f.impact.label = t!("check.failing_query.impact", share = &share);
    f.confidence = Confidence::High;
    f.metrics = vec![
        Metric::new(t!("common.metric.runs"), q.executions, Unit::Count),
        Metric::new(t!("check.failing_query.metric.unfinished"), rate, Unit::Ratio),
    ];
    if aborted > 0.0 {
        f.metrics.push(Metric::new(t!("check.failing_query.metric.cancelled"), aborted, Unit::Count));
    }
    if failed > 0.0 {
        f.metrics.push(Metric::new(t!("check.failing_query.metric.errors"), failed, Unit::Count));
    }
    if let Some(ms) = costs.aborted_mean_ms {
        f.metrics.push(Metric::new(t!("check.failing_query.metric.cancelled_after"), ms, Unit::Ms));
    }
    if let Some(e) = &costs.top_error {
        f.metrics.push(Metric::text(t!("check.failing_query.metric.recent_error"), e.clone()));
    }
    f.fix = Some(failing_fix(snapshot, &runs));
    // On running totals the failures hold every run from before the change: two scans show it.
    let now = t!(
        "check.failing_query.verify.now",
        unfinished = format::count(unfinished),
        runs = format::count(q.executions)
    );
    f.verify = vec![match crate::queries::running_totals(snapshot) {
        Some(source) => t!("check.failing_query.verify.between_scans", source = source, now = now),
        None => t!("check.failing_query.verify.rescan"),
    }];
    if !runs.recent {
        f.caveats.push(t!("check.failing_query.caveat.maybe_old", engine = engine_name(snapshot)));
    } else if !mssql {
        f.caveats.push(t!("check.failing_query.caveat.since_entry", engine = engine_name(snapshot)));
    }
    f
}

fn failing_fix(snapshot: &Snapshot, runs: &FailingRuns<'_>) -> Fix {
    let (q, costs) = (runs.q, runs.costs);
    match runs.fix {
        FailingFix::Timeout => Fix {
            summary: match runs.timeout {
                Some(s) => t!("check.failing_query.fix.timeout_seen", seconds = format!("{s:.0}")),
                None => t!("check.failing_query.fix.timeout"),
            },
            sql: None,
            cost: None,
        },
        FailingFix::CaptureErrors => {
            let azure = snapshot.server.on(culprit_core::Platform::AzureSqlDatabase);
            // One session catches every failing statement's errors: each finding's fix starts the same one, once.
            let start = super::event_session_sql(
                "culprit_errors",
                azure,
                &format!(
                    "ADD EVENT sqlserver.error_reported (\n  ACTION (sqlserver.client_app_name, sqlserver.sql_text)\n  WHERE severity >= 11 AND sqlserver.database_name = N'{}')\nADD TARGET package0.ring_buffer",
                    snapshot.server.database.replace('\'', "''")
                ),
            );
            Fix {
                summary: t!("check.failing_query.fix.capture_errors"),
                sql: Some(format!(
                    "-- Capture errors raised in this database (one session for every failing statement: once is enough)\n{start}\n{}",
                    super::event_session_drop("culprit_errors", azure)
                )),
                cost: None,
            }
        }
        FailingFix::Error(code) => {
            // The holder is found the way every check that names a lock wait finds it: Live only for timeouts that come
            // often enough to catch one.
            let holder = matches!(code, ErrorCode::LockWaitTimeout)
                .then(|| find_lock_holder(snapshot, q, costs.failed.filter(|n| *n > 0.0)));
            let advice = match (code, &holder) {
                (ErrorCode::LockWaitTimeout, Some(h)) => {
                    t!("check.failing_query.fix.lock_wait_timeout", lead = &h.lead)
                }
                (ErrorCode::LockWaitTimeout, None) => String::new(),
                (ErrorCode::Deadlock, _) => t!("check.failing_query.fix.deadlock"),
                (ErrorCode::StatementTimeout, _) => t!("check.failing_query.fix.statement_timeout"),
                (ErrorCode::Duplicate, _) => t!("check.failing_query.fix.duplicate"),
                (ErrorCode::Other, _) => t!("check.failing_query.fix.read_errors"),
            };
            let errors = q.query_hash.as_ref().map(|digest| {
                format!(
                    "SELECT MYSQL_ERRNO, RETURNED_SQLSTATE, MESSAGE_TEXT\n  FROM performance_schema.events_statements_history\n WHERE DIGEST = '{}' AND MYSQL_ERRNO <> 0;",
                    digest.replace('\'', "")
                )
            });
            // The settings that log the next lock waits, where they are too rare to catch, after the read.
            let logging = holder.and_then(|h| h.sql);
            Fix {
                summary: format!("{advice} {}", t!("check.failing_query.fix.errors_kept")),
                sql: match (errors, logging) {
                    (Some(errors), Some(logging)) => Some(format!("{errors}\n\n{logging}")),
                    (errors, logging) => errors.or(logging),
                },
                cost: None,
            }
        }
    }
}
