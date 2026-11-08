//! The `row-lock-waits` check: InnoDB row-lock waits add up to real time (a minute an hour or more across the server), from long transactions or updates that scan instead of using an index.

use super::*;
use crate::evidence::Requirement as Evidence;

pub fn row_lock_waits_info() -> CheckInfo {
    CheckInfo {
        id: "row-lock-waits".into(),
        title: t!("check.row_lock_waits.info.title"),
        category: Category::Queries,
        engines: vec![Engine::Mysql],
        requires: vec![],
        description: t!("check.row_lock_waits.info.description"),
    }
}

check!(
    RowLockWaitsCheck: row_lock_waits_info,
    decide = row_lock_time,
    render = row_lock_waits_finding,
    case<'s> = RowLockWaits<'s>,
    evidence = &[Evidence::Pressure {
        signal: Some(culprit_core::signal_id::ROW_LOCK_AVG),
        timed_files: false,
        complete: true
    }],
);

/// The row-lock-waits findings of `snapshot`: its cases, each worded.
#[cfg(test)]
pub(crate) fn row_lock_waits(snapshot: &Snapshot) -> Vec<Finding> {
    culprit_core::Check::run(&RowLockWaitsCheck, snapshot)
}

/// The server's row-lock waits since it started, a minute an hour or more, and what of them this database shows.
pub struct RowLockWaits<'s> {
    /// Waits since the start.
    count: f64,
    /// Their time since the start, and on average.
    total_ms: f64,
    avg_ms: f64,
    per_hour_ms: f64,
    /// That time against this database's statement time an hour.
    share: Option<f64>,
    /// This database's statement that waited longest on locks, and how long.
    top: Option<(&'s QueryStat, f64)>,
    /// That statement spends most of its time waiting on locks: it is where they show, named with the finding.
    named: Option<&'s QueryStat>,
    severity: Severity,
}

fn row_lock_time(snapshot: &Snapshot) -> Vec<RowLockWaits<'_>> {
    let Some(avg_ms) = signal(snapshot, culprit_core::signal_id::ROW_LOCK_AVG) else { return Vec::new() };
    // A rate needs at least an hour of counters.
    let Some(hours) = counter_hours(snapshot) else { return Vec::new() };
    let Some(waits) = pressure(snapshot).and_then(|p| p.signal(culprit_core::signal_id::ROW_LOCK_WAITS)) else {
        return Vec::new();
    };
    let count = if waits.unit == SignalUnit::PerHour { waits.value * hours } else { waits.value };
    let Some(total_ms) = signal(snapshot, culprit_core::signal_id::ROW_LOCK_TIME) else { return Vec::new() };
    let per_hour_ms = total_ms / hours;
    // What it costs is time, not how many waits: 61 waits of half a second an hour is half a minute.
    if per_hour_ms < MINUTE_MS {
        return Vec::new();
    }
    // Against this database's statement time an hour (the server's waits include every database's).
    let share = snapshot
        .workload
        .as_ref()
        .filter(|w| w.measurement_hours() >= culprit_core::RATE_MIN_HOURS && w.total_time_ms > 0.0)
        .map(|w| per_hour_ms / (w.total_time_ms / w.measurement_hours()));
    // InnoDB reads a plain SELECT from a snapshot, taking no row locks (except under SERIALIZABLE, which makes it a
    // locking read): its lock time is table and metadata locks, never these waits.
    let serializable = ["transaction_isolation", "tx_isolation"]
        .iter()
        .any(|n| snapshot.setting(n).is_some_and(|s| s.value.to_ascii_uppercase().contains("SERIALIZABLE")));
    let takes_row_locks = |q: &QueryStat| serializable || !culprit_core::sql::reads_only(&q.text, Engine::Mysql);
    // A statement whose own time is mostly lock waits is where they show; one with a little lock time isn't named
    // as their cause, nor one whose lock time MariaDB made of its whole run.
    let top = app_queries(snapshot)
        .filter(|q| takes_row_locks(q) && !crate::signals::lock_time_counts_the_run(snapshot, q))
        .filter_map(|q| {
            let lock: f64 = q.waits.iter().filter(|w| category_key(&w.category) == "lock").map(|w| w.wait_ms).sum();
            (lock > 0.0).then_some((q, lock))
        })
        .max_by(|a, b| a.1.total_cmp(&b.1));
    let named = top.filter(|(q, ms)| q.total_time_ms > 0.0 && ms / q.total_time_ms >= 0.5).map(|(q, _)| q);
    // Totals since the start describe the past: at most Medium, and Medium only when they are a real part of the
    // work (ten minutes an hour, or a tenth of this database's statement time) and this database's own statements
    // show it (one spends most of its time waiting on locks): the counters are every database's on the server.
    let severity = if named.is_some() && (per_hour_ms >= 10.0 * MINUTE_MS || share.is_some_and(|s| s >= 0.1)) {
        Severity::Medium
    } else {
        Severity::Low
    };
    vec![RowLockWaits { count, total_ms, avg_ms, per_hour_ms, share, top, named, severity }]
}

fn row_lock_waits_finding(snapshot: &Snapshot, r: RowLockWaits<'_>) -> Finding {
    let mut f = finding(
        "row-lock-waits",
        &server_key(snapshot),
        Category::Queries,
        subject(SubjectKind::Database, snapshot.server.database.clone()),
    );
    let total = crate::signals::total_wait(r.total_ms);
    let average = format::duration(r.avg_ms);
    f.title = t!("check.row_lock_waits.title", total = &total, since = uptime_phrase(snapshot), average = &average);
    let since = uptime_opening(snapshot);
    let (count, per_hour) = (format::count(r.count), format::duration(r.per_hour_ms));
    let mut summary = vec![match r.share {
        Some(s) => t!(
            "check.row_lock_waits.summary.waits_share",
            since = since,
            count = count,
            total = &total,
            average = &average,
            per_hour = per_hour,
            share = format::percent(s.min(9.99))
        ),
        None => t!(
            "check.row_lock_waits.summary.waits",
            since = since,
            count = count,
            total = &total,
            average = &average,
            per_hour = per_hour
        ),
    }];
    if let Some((q, ms)) = r.top {
        summary.push(t!("check.row_lock_waits.summary.longest", query = query_handle(q), time = format::duration(ms)));
    }
    summary.push(t!("check.row_lock_waits.summary.causes"));
    f.summary = summary.join(" ");
    if let Some(q) = r.named {
        f.queries = query_refs(&[q]);
    }
    f.severity = r.severity;
    f.impact.score = base_score(f.severity);
    f.impact.label = t!("check.row_lock_waits.impact", per_hour = format::duration(r.per_hour_ms));
    f.confidence = Confidence::Medium;
    f.metrics = vec![
        Metric::new(t!("check.row_lock_waits.metric.waits"), r.count, Unit::Count),
        Metric::said(key!("check.row_lock_waits.metric.time"), r.total_ms, Unit::Ms),
        Metric::new(t!("check.row_lock_waits.metric.average"), r.avg_ms, Unit::Ms),
    ];
    f.caveats.push(t!("check.row_lock_waits.caveat.server_wide", engine = snapshot.server.name()));
    f.fix = Some(Fix { summary: t!("check.row_lock_waits.fix"), sql: None, cost: None });
    f.verify = vec![t!("check.row_lock_waits.verify")];
    f
}
