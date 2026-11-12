//! The `logged-errors` check: SQL errors an application's structured query log recorded, by their code.

use super::*;
use crate::evidence::{Observation, Requirement as Evidence};
use culprit_core::operations::LoggedFailure;
use culprit_core::{Actionability, t};

pub fn logged_errors_info() -> CheckInfo {
    CheckInfo {
        id: "logged-errors".into(),
        title: t!("check.logged_errors.info.title"),
        category: Category::Queries,
        engines: vec![Engine::Sqlite],
        requires: vec![Capability::QueryLog],
        description: t!("check.logged_errors.info.description"),
    }
}

check!(
    LoggedErrorsCheck: logged_errors_info,
    decide = logged_errors,
    render = logged_error_finding,
    case<'s> = LoggedError<'s>,
    evidence = &[Evidence::Observed(Observation::LogFailures, &[])],
);

/// An error code the application's log recorded for a statement, and what it establishes.
pub struct LoggedError<'s> {
    failure: &'s LoggedFailure,
    /// What the code means, which says what to do about it.
    code: ErrorCode,
    /// A storage or integrity error dated within the scan's window: it needs attention.
    critical: bool,
}

/// The codes Culprit knows the meaning of.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ErrorCode {
    Busy,
    Locked,
    BusySnapshot,
    Constraint,
    Full,
    ReadOnly,
    /// SQLITE_IOERR and SQLITE_CORRUPT.
    Storage,
    Unrecognized,
}

impl ErrorCode {
    fn of(code: &str) -> Self {
        match code {
            "SQLITE_BUSY" => ErrorCode::Busy,
            "SQLITE_LOCKED" => ErrorCode::Locked,
            "SQLITE_BUSY_SNAPSHOT" => ErrorCode::BusySnapshot,
            "SQLITE_CONSTRAINT" => ErrorCode::Constraint,
            "SQLITE_FULL" => ErrorCode::Full,
            "SQLITE_READONLY" => ErrorCode::ReadOnly,
            "SQLITE_IOERR" | "SQLITE_CORRUPT" => ErrorCode::Storage,
            _ => ErrorCode::Unrecognized,
        }
    }

    /// What the code says happened.
    fn why(self) -> String {
        match self {
            ErrorCode::Busy => t!("check.logged_errors.why.busy"),
            ErrorCode::Locked => t!("check.logged_errors.why.locked"),
            ErrorCode::BusySnapshot => t!("check.logged_errors.why.busy_snapshot"),
            ErrorCode::Constraint => t!("check.logged_errors.why.constraint"),
            ErrorCode::Full => t!("check.logged_errors.why.full"),
            ErrorCode::ReadOnly => t!("check.logged_errors.why.readonly"),
            ErrorCode::Storage => t!("check.logged_errors.why.storage"),
            ErrorCode::Unrecognized => t!("check.logged_errors.why.unrecognized"),
        }
    }

    /// What to do about it.
    fn fix(self) -> String {
        match self {
            ErrorCode::Busy => t!("check.logged_errors.fix.busy"),
            ErrorCode::Locked => t!("check.logged_errors.fix.locked"),
            ErrorCode::BusySnapshot => t!("check.logged_errors.fix.busy_snapshot"),
            ErrorCode::Constraint => t!("check.logged_errors.fix.constraint"),
            ErrorCode::Full => t!("check.logged_errors.fix.full"),
            ErrorCode::ReadOnly => t!("check.logged_errors.fix.readonly"),
            ErrorCode::Storage => t!("check.logged_errors.fix.storage"),
            ErrorCode::Unrecognized => t!("check.logged_errors.fix.unrecognized"),
        }
    }
}

/// Every code the log recorded at least once; storage and integrity errors dated within the window need attention.
fn logged_errors(s: &Snapshot) -> Vec<LoggedError<'_>> {
    let now = parse_epoch_seconds(&s.taken_at);
    let window_s = f64::from(s.options.as_ref().map_or(24 * 60, |o| o.window_in_minutes())) * 60.0;
    s.operations
        .log_failures
        .iter()
        .filter(|e| e.count > 0.0)
        .map(|failure| {
            let recent = failure
                .last_at
                .as_deref()
                .and_then(parse_epoch_seconds)
                .zip(now)
                .is_some_and(|(at, now)| at <= now && at >= now - window_s);
            let code = ErrorCode::of(&failure.code);
            LoggedError { failure, code, critical: recent && matches!(code, ErrorCode::Full | ErrorCode::Storage) }
        })
        .collect()
}

fn logged_error_finding(s: &Snapshot, error: LoggedError<'_>) -> Finding {
    let LoggedError { failure: e, code, critical } = error;
    let mut f = finding(
        "logged-errors",
        &format!("{}:{}", e.query_id, e.code),
        Category::Queries,
        subject(SubjectKind::Query, &e.query_id),
    );
    f.facts.workload_dependency = Some(culprit_core::WorkloadDependency::Independent);
    let count = format::count(e.count);
    f.title = t!("check.logged_errors.title", code = &e.code, count = count.clone());
    let dated = match e.last_at.as_deref() {
        Some(at) => t!("check.logged_errors.summary.latest", at = at),
        None => t!("check.logged_errors.summary.undated"),
    };
    f.summary =
        [code.why(), t!("check.logged_errors.summary.records"), dated, t!("check.logged_errors.summary.durations")]
            .join(" ");
    (f.severity, f.actionability) = if critical {
        (Severity::Medium, Actionability::NeedsAttention)
    } else {
        (Severity::Low, Actionability::Information)
    };
    f.confidence = Confidence::High;
    f.impact.score = base_score(f.severity);
    f.impact.set_label(t!("check.logged_errors.impact", count = count));
    f.metrics = vec![
        Metric::said_text(culprit_core::key!("check.logged_errors.metric.code"), &e.code),
        Metric::said_text(culprit_core::key!("check.logged_errors.metric.statement"), &e.statement),
        Metric::said(culprit_core::key!("check.logged_errors.metric.records"), e.count, Unit::Count),
    ];
    if let Some(q) = s.workload.as_ref().and_then(|w| w.query(&e.query_id)) {
        f.queries = crate::support::query_refs(&[q]);
    }
    f.fix = Some(Fix { summary: code.fix(), sql: None, cost: None });
    f.verify = vec![t!("check.logged_errors.verify")];
    f
}
