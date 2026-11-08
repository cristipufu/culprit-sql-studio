//! The `lock-time` check: Statements that spend a large part of their time waiting for locks other sessions hold: Query Store's lock waits per query, MySQL and MariaDB lock time per digest. PostgreSQL keeps no lock time per statement; Live shows blocking as it happens.

use super::*;
use crate::evidence::{QueryMeasure, Requirement as Evidence};

pub fn lock_time_info() -> CheckInfo {
    CheckInfo {
        id: "lock-time".into(),
        title: t!("check.lock_time.info.title"),
        category: Category::Queries,
        engines: vec![Engine::Mssql, Engine::Mysql],
        requires: vec![],
        description: t!("check.lock_time.info.description"),
    }
}

check!(
    LockTimeCheck: lock_time_info,
    decide = lock_waits,
    render = lock_time_finding,
    case<'s> = LockWait<'s>,
    evidence = &[Evidence::Query(QueryMeasure::Waits("Lock"))],
);

/// Lock waits a day that make a statement's waiting High, or Medium: half an hour and five minutes.
pub(super) const LOCK_HIGH_MS_PER_DAY: f64 = 30.0 * 60_000.0;
const LOCK_MEDIUM_MS_PER_DAY: f64 = 5.0 * 60_000.0;

/// Runs that end in lock wait timeouts often enough for failing-query to report them.
pub(super) fn times_out_on_locks(q: &QueryStat) -> bool {
    q.costs.as_ref().is_some_and(|c| {
        c.top_error.as_deref().is_some_and(|e| e.starts_with("1205"))
            && c.unfinished().is_some_and(|n| n >= 5.0 && per_run(n, q.executions) >= 0.01)
    })
}

/// Whether MariaDB counted this statement's whole run as its lock time. With the slow log writing to a table
/// (`log_output` with TABLE), a statement slower than long_query_time reports a lock time of about its whole run: the
/// demo's GROUP BY over a million rows a run showed 49,963.3 ms of lock in 49,963.9 ms, and 9.6 ms in a scan with the
/// slow log off. Where long_query_time was lower for part of the window, the same shows as a statement that examines
/// thousands of rows a run in almost no time besides its lock wait. Its lock time says nothing then.
pub(crate) fn lock_time_counts_the_run(snapshot: &Snapshot, q: &QueryStat) -> bool {
    let setting = |name: &str| snapshot.setting(name).map(|s| s.value.trim().to_ascii_uppercase());
    let slow_log_to_table = setting("log_output").is_some_and(|v| v.contains("TABLE"))
        && ["slow_query_log", "log_slow_query"].iter().any(|n| setting(n).is_some_and(|v| crate::config::is_on(&v)));
    if !is_mariadb(snapshot) || !slow_log_to_table {
        return false;
    }
    let logged = setting("long_query_time")
        .and_then(|v| v.parse::<f64>().ok())
        .is_some_and(|seconds| q.max_time_ms.unwrap_or(q.mean_time_ms) >= seconds * 1_000.0);
    let examined = q.logical_reads.map(|r| per_run(r, q.executions)).unwrap_or(0.0);
    logged || (lock_ms(q) >= 0.99 * q.total_time_ms && examined >= 10_000.0)
}

/// A statement that spends a quarter or more of its time, and ten seconds or more in all, waiting for locks.
pub struct LockWait<'s> {
    q: &'s QueryStat,
    lock: f64,
    /// Lock time against the statement's own time.
    share: f64,
    per: PerRun,
    severity: Severity,
    /// Its lock time against the database's.
    db_share: f64,
    /// The window doesn't measure minutes a day of it (an hour or more from a known start).
    no_rate: bool,
    /// MySQL before 8.0.28 counted only table locks in a statement's lock time.
    table_locks_only_before_8_0_28: bool,
    /// A read that waits for writers because READ_COMMITTED_SNAPSHOT is off: row versioning removes the wait.
    versioning: bool,
}

/// How the wait of a statement's runs is said: what happened to one or two runs, never an average of 5.2 s for runs
/// of 10.5 s and a fraction of a millisecond.
#[derive(Clone, Copy)]
pub enum PerRun {
    /// Its one run waited this long.
    One(f64),
    /// The longer of two runs waited this long, or at least this long when some of the wait may be the other's.
    Longest { waited: f64, longest: f64, at_least: bool },
    /// On average a run.
    Average(f64),
}

fn lock_waits(snapshot: &Snapshot) -> Vec<LockWait<'_>> {
    let total_db = snapshot.workload.as_ref().map(|w| w.total_time_ms).unwrap_or(0.0);
    let mut cases: Vec<(LockWait<'_>, f64)> = app_queries(snapshot)
        // A statement that names no table (SELECT SLEEP(…)) isn't waiting on the application's rows.
        .filter(|q| !matches!(query_handle_of(q), Handle::Query))
        // Lock wait timeouts are already the failing-query finding, with the same fix.
        .filter(|q| !times_out_on_locks(q))
        // A lock time MariaDB made of the statement's whole run isn't waiting.
        .filter(|q| !lock_time_counts_the_run(snapshot, q))
        .filter_map(|q| {
            let lock = lock_ms(q);
            let share = per_run(lock, q.total_time_ms);
            (lock >= 10_000.0 && share >= 0.25).then_some((q, lock, share.min(1.0)))
        })
        .map(|(q, lock, share)| {
            // One run, or two of which one took twice the other. The longer of two waited at least what the other
            // can't have.
            let per = match q.few_runs().as_deref() {
                Some([_]) => PerRun::One(lock),
                Some([longest, other]) => PerRun::Longest {
                    waited: (lock - other).clamp(0.0, *longest),
                    longest: *longest,
                    at_least: share < 0.95,
                },
                _ => PerRun::Average(per_run(lock, q.executions)),
            };
            let db_share = if total_db > 0.0 { lock / total_db } else { 0.0 };
            // A share of database time alone makes one two-minute incident in a quiet database High: the waiting must
            // also add up to minutes a day, measured over an hour or more.
            let rate = window_rate_per_day(snapshot, q, lock);
            let by_minutes = since_start_severity(rate, LOCK_HIGH_MS_PER_DAY, LOCK_MEDIUM_MS_PER_DAY);
            let severity = severity_for_share(db_share).min(by_minutes);
            let case = LockWait {
                q,
                lock,
                share,
                per,
                severity,
                db_share,
                no_rate: rate.is_none(),
                table_locks_only_before_8_0_28: snapshot.server.engine == Engine::Mysql && !is_mariadb(snapshot),
                // A read that waits for writers' locks because reads don't use row versions here: that setting is the
                // change that removes the wait, as config:read-committed-snapshot says.
                versioning: crate::config::read_committed_snapshot_off(snapshot)
                    && crate::config::versioning_spares_read(q),
            };
            (case, score_for_share(db_share).max(base_score(severity)))
        })
        .collect();
    cases.sort_by(|a, b| b.1.total_cmp(&a.1));
    cases.truncate(5);
    cases.into_iter().map(|(case, _)| case).collect()
}

fn lock_time_finding(snapshot: &Snapshot, wait: LockWait<'_>) -> Finding {
    let LockWait { q, lock, share, per, .. } = wait;
    let mut f = finding("lock-time", &q.id, Category::Queries, subject(SubjectKind::Query, &q.id));
    let handle = capitalize(&query_handle(q));
    // A statement that stopped running is told in the past.
    let tense = tense(snapshot, &[q]);
    let (per, waited) = match per {
        PerRun::One(ms) => ("one", ms),
        PerRun::Longest { waited, at_least: false, .. } => ("two", waited),
        PerRun::Longest { waited, at_least: true, .. } => ("at_least", waited),
        PerRun::Average(ms) => ("average", ms),
    };
    f.title = if share >= 0.95 {
        t!(
            "check.lock_time.title.nearly_all",
            handle = &handle,
            tense = tense,
            per = per,
            waited = format::duration(waited)
        )
    } else {
        t!(
            "check.lock_time.title.share",
            handle = &handle,
            tense = tense,
            share = format::percent(share),
            per = per,
            waited = format::duration(waited)
        )
    };
    let mut summary = vec![t!(
        "check.lock_time.summary.waited",
        total = format::duration(q.total_time_ms),
        ran = ran_phrase(snapshot, q),
        lock = format::duration(lock)
    )];
    if wait.table_locks_only_before_8_0_28 {
        summary.push(t!("check.lock_time.summary.mysql_before_8_0_28"));
    }
    f.summary = summary.join(" ");
    f.severity = wait.severity;
    if wait.no_rate {
        f.caveats.push(t!("check.lock_time.caveat.no_rate"));
    }
    with_query(snapshot, &mut f, q);
    f.impact.score = score_for_share(wait.db_share).max(base_score(f.severity));
    f.impact.set_label(t!("check.lock_time.impact", lock = format::duration(lock)));
    f.confidence = Confidence::High;
    f.metrics = vec![
        Metric::said(culprit_core::key!("check.lock_time.metric.lock_wait"), lock, Unit::Ms),
        Metric::said(culprit_core::key!("check.lock_time.metric.share"), share, Unit::Ratio),
        match wait.per {
            PerRun::Longest { longest, .. } => {
                Metric::said(culprit_core::key!("check.lock_time.metric.longest_run"), longest, Unit::Ms)
            }
            _ => Metric::said(
                culprit_core::key!("check.lock_time.metric.per_run"),
                per_run(lock, q.executions),
                Unit::Ms,
            ),
        },
    ];
    f.facts.lock_wait =
        Some(culprit_core::FindingLockWait { ms: lock, share, versioning_spares_read: wait.versioning });
    // How to find the holder, as every check that names a lock wait says it: Live only for waits that come often
    // enough to catch one.
    let LockHolder { lead, sql } = find_lock_holder(snapshot, q, None);
    f.fix = Some(Fix {
        // Caught live, the holder is found "without it" in the same sentence; otherwise the advice follows whole.
        summary: match (wait.versioning, catches_live(snapshot, q, None)) {
            (true, true) => t!("check.lock_time.fix.versioning_live"),
            (true, false) => t!("check.lock_time.fix.versioning", lead = &lead),
            (false, _) => t!("check.lock_time.fix.holders", lead = &lead),
        },
        sql,
        cost: None,
    });
    // On running totals the lock time holds every run from before the change: two scans show it.
    let now = t!(
        "check.lock_time.verify.now",
        lock = format::duration(lock),
        n = q.executions,
        runs = format::count(q.executions)
    );
    f.verify = vec![match crate::queries::running_totals(snapshot) {
        Some(source) => t!("check.lock_time.verify.between_scans", source = source, now = now),
        None => t!("check.lock_time.verify.rescan"),
    }];
    f
}
