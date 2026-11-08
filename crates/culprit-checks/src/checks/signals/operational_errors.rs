use super::*;
use crate::evidence::{Observation, Requirement as Evidence};
use culprit_core::operations::{CounterReading, OperationalCounter, OperationalCounterKind as Kind};
use culprit_core::{Actionability, FileScope, t};

pub fn operational_errors_info() -> CheckInfo {
    CheckInfo {
        id: "operational-errors".into(),
        title: t!("check.operational_errors.info.title"),
        category: Category::Maintenance,
        engines: vec![Engine::Postgres, Engine::Mysql, Engine::Mssql],
        requires: vec![],
        description: t!("check.operational_errors.info.description"),
    }
}

check!(
    OperationalErrorsCheck: operational_errors_info,
    decide = error_counters,
    render = operational_errors_finding,
    case<'s> = ErrorCounter<'s>,
    evidence = &[Evidence::Observed(
        Observation::OperationalCounters,
        &[
            culprit_core::source_id::ARCHIVER,
            culprit_core::source_id::SUBSCRIPTION_ERRORS,
            culprit_core::source_id::STANDBY_CONFLICTS,
            culprit_core::source_id::REDO_PRESSURE,
        ],
    )],
);

/// A counter of failures or waits that recorded any, and what it establishes.
pub struct ErrorCounter<'s> {
    counter: &'s OperationalCounter,
    /// The earlier reading inside the requested period it compares with, and the events counted since.
    recent: Option<(&'s CounterReading, f64)>,
    /// Counted for the whole server: context, never graded on this database.
    shared: bool,
    /// Pages of this database that failed their checksum or a read.
    corruption: bool,
    /// New events between scans, or corruption: something to attend to now.
    current: bool,
}

fn error_counters(s: &Snapshot) -> Vec<ErrorCounter<'_>> {
    let now = parse_epoch_seconds(&s.taken_at);
    let window_secs = f64::from(s.options.as_ref().map_or(24 * 60, |o| o.window_in_minutes())) * 60.0;
    let start = now.map(|n| n - window_secs * 1.25);
    s.operations
        .counters
        .iter()
        .filter_map(|c| {
            if !c.count.is_finite() || c.count <= 0.0 {
                return None;
            }
            let recent = c.before.as_ref().filter(|b| {
                parse_epoch_seconds(&b.at).zip(start).is_some_and(|(a, start)| a >= start)
                    && parse_epoch_seconds(&b.at).zip(now).is_some_and(|(a, n)| a < n)
                    && c.count >= b.count
                    && (c.count > b.count
                        || parse_epoch_seconds(&b.at).zip(now).is_some_and(|(a, n)| n - a >= window_secs * 0.8))
            });
            let delta = recent.map(|b| c.count - b.count);
            let shared = c.scope == FileScope::Server;
            let corruption = c.kind == Kind::ChecksumFailures && !shared;
            let current = corruption || (delta.is_some_and(|d| d >= 10.0) && !shared);
            if delta == Some(0.0) && !corruption {
                return None;
            }
            Some(ErrorCounter {
                counter: c,
                recent: recent.map(|b| (b, c.count - b.count)),
                shared,
                corruption,
                current,
            })
        })
        .collect()
}

fn operational_errors_finding(s: &Snapshot, e: ErrorCounter<'_>) -> Finding {
    let c = e.counter;
    let (key, label, why, fix) = match c.kind {
        Kind::ArchiveFailures => (
            "archive",
            t!("check.operational_errors.label.archive"),
            t!("check.operational_errors.why.archive", subject = &c.subject),
            t!("check.operational_errors.fix.archive"),
        ),
        Kind::SubscriptionApplyErrors => (
            "subscription-apply",
            t!("check.operational_errors.label.subscription_apply"),
            t!("check.operational_errors.why.subscription_apply", subject = &c.subject),
            t!("check.operational_errors.fix.subscription_apply"),
        ),
        Kind::SubscriptionSyncErrors => (
            "subscription-sync",
            t!("check.operational_errors.label.subscription_sync"),
            t!("check.operational_errors.why.subscription_sync", subject = &c.subject),
            t!("check.operational_errors.fix.subscription_sync"),
        ),
        Kind::StandbyConflicts => (
            "standby-conflicts",
            t!("check.operational_errors.label.standby_conflicts"),
            t!("check.operational_errors.why.standby_conflicts", subject = &c.subject),
            t!("check.operational_errors.fix.standby_conflicts"),
        ),
        Kind::RedoLogWaits => (
            "redo-waits",
            t!("check.operational_errors.label.redo_waits"),
            t!("check.operational_errors.why.redo_waits", subject = &c.subject),
            t!("check.operational_errors.fix.redo_waits"),
        ),
        Kind::ChecksumFailures => (
            "checksum-failures",
            t!("check.operational_errors.label.checksum_failures"),
            t!("check.operational_errors.why.checksum_failures", subject = &c.subject),
            t!("check.operational_errors.fix.checksum_failures"),
        ),
        Kind::ConnectionFailures => (
            "connection-failures",
            t!("check.operational_errors.label.connection_failures"),
            t!("check.operational_errors.why.connection_failures", subject = &c.subject),
            t!("check.operational_errors.fix.connection_failures"),
        ),
    };
    let delta = e.recent.map(|(_, d)| d);
    let mut f = finding(
        "operational-errors",
        &format!("{key}:{}", c.subject),
        Category::Maintenance,
        subject(SubjectKind::Database, &c.subject),
    );
    f.title = match delta {
        Some(delta) => t!("check.operational_errors.title.new", label = &label, count = format::count(delta)),
        None => t!("check.operational_errors.title.history", label = &label, count = format::count(c.count)),
    };
    let mut summary = vec![why];
    summary.push(match c.since.as_deref() {
        Some(since) => t!("check.operational_errors.summary.since", since = since),
        None => t!("check.operational_errors.summary.period_unknown"),
    });
    if let Some((b, _)) = e.recent {
        summary.push(t!("check.operational_errors.summary.compared", from = &b.at, to = &s.taken_at));
    }
    summary.push(if e.shared {
        t!("check.operational_errors.summary.shared")
    } else if e.corruption {
        t!("check.operational_errors.summary.corruption")
    } else if e.recent.is_none() {
        t!("check.operational_errors.summary.history")
    } else {
        t!("check.operational_errors.summary.new")
    });
    f.summary = summary.join(" ");
    f.severity = if e.corruption {
        Severity::High
    } else if e.current {
        Severity::Medium
    } else {
        Severity::Low
    };
    f.actionability = if e.current { Actionability::NeedsAttention } else { Actionability::Information };
    f.confidence = Confidence::High;
    f.impact.score = base_score(f.severity);
    f.impact.label = if let Some(d) = delta {
        t!("check.operational_errors.impact.new", count = format::count(d))
    } else if e.corruption {
        t!("check.operational_errors.impact.corrupt_pages", count = format::count(c.count))
    } else {
        t!("check.operational_errors.impact.history")
    };
    f.metrics = vec![Metric::new(t!("check.operational_errors.metric.total"), c.count, Unit::Count)];
    if let Some(d) = delta {
        f.metrics.push(Metric::new(t!("check.operational_errors.metric.new"), d, Unit::Count));
    }
    if let Some(a) = &c.last_event_at {
        f.metrics.push(Metric::text(t!("check.operational_errors.metric.latest"), a));
    }
    f.fix = Some(Fix { summary: fix, sql: None, cost: None });
    f.verify = vec![t!("check.operational_errors.verify")];
    f
}
