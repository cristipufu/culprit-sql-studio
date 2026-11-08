//! The `memory-queue` check: Waiting for memory takes a real amount of time (a minute an hour or more), or queries in this database were queueing for a memory grant when the scan ran.

use super::*;
use crate::evidence::Requirement as Evidence;

pub fn memory_queue_info() -> CheckInfo {
    CheckInfo {
        id: "memory-queue".into(),
        title: t!("check.memory_queue.info.title"),
        category: Category::Configuration,
        engines: vec![Engine::Mssql],
        requires: vec![],
        description: t!("check.memory_queue.info.description"),
    }
}

check!(
    MemoryQueueCheck: memory_queue_info,
    decide = memory_queues,
    render = memory_queue_finding,
    case<'s> = MemoryQueue,
    evidence = &[Evidence::Pressure {
        signal: Some(culprit_core::signal_id::MEMORY_GRANTS_PENDING),
        timed_files: false,
        complete: true
    }],
);

/// The memory-queue findings of `snapshot`: its cases, each worded.
#[cfg(test)]
pub(crate) fn memory_queue(snapshot: &Snapshot) -> Vec<Finding> {
    culprit_core::Check::run(&MemoryQueueCheck, snapshot)
}

/// Queries waiting for memory: on record over a measured period, from grant waits since the start, or queueing when
/// the scan ran.
pub struct MemoryQueue {
    /// Memory waits' share of all waiting, and their time an hour.
    share: f64,
    memory_per_hour: f64,
    /// Memory waits are a real part of the waiting on record: 5% of it, a minute an hour or more.
    history: bool,
    /// The waits are Query Store's window for this database, not the server's totals since it started.
    windowed: bool,
    /// The waits are the instance's totals since it started, every database's.
    shared: bool,
    /// Grant waits (RESOURCE_SEMAPHORE) since the start, and an hour.
    grant_ms: f64,
    grants_per_hour: f64,
    /// Grant waits since the start are this database's (on Azure SQL Database), a minute an hour or more.
    grants: bool,
    /// Queries in this database waiting for a grant when the scan ran.
    pending: f64,
    /// Grants explain the waiting (grant waits on record, or queries queueing for one now); otherwise the waits may be
    /// grants or allocations, and the advice is to sample them.
    grant_story: bool,
    /// Seen once, when the scan ran, with nothing on record before it.
    seen_once: bool,
    severity: Severity,
    confidence: Confidence,
}

fn memory_queues(snapshot: &Snapshot) -> Vec<MemoryQueue> {
    let total: f64 = snapshot.waits.iter().map(|w| w.wait_ms).sum();
    let memory: f64 = snapshot.waits.iter().filter(|w| category_key(&w.category) == "memory").map(|w| w.wait_ms).sum();
    let share = if total > 0.0 { memory / total } else { 0.0 };
    // A share needs an amount behind it: 6% of a few seconds of waiting over hours is nobody queueing.
    let (window, windowed) = waits_window(snapshot);
    let memory_per_hour = window.map(|h| memory / h);
    let history = share >= 0.05 && memory_per_hour.is_some_and(|m| m >= MINUTE_MS);
    // Memory grants alone (RESOURCE_SEMAPHORE), from the wait totals since the server started. The Memory category
    // also holds allocation waits (CMEMTHREAD, MEMORY_ALLOCATION_EXT), which no grant change helps.
    let grant_ms = signal(snapshot, culprit_core::signal_id::GRANT_WAIT_MS).unwrap_or(0.0);
    let grants_per_hour = counter_hours(snapshot).map(|h| grant_ms / h);
    let instance = !snapshot.server.on(culprit_core::Platform::AzureSqlDatabase);
    let grants = !instance && grants_per_hour.is_some_and(|g| g >= MINUTE_MS);
    let pending = signal(snapshot, culprit_core::signal_id::MEMORY_GRANTS_PENDING).unwrap_or(0.0);
    // Instance-wide grant/allocation totals cannot establish queueing in this database.
    if instance && !windowed && pending <= 0.0 {
        return Vec::new();
    }
    if !history && !grants && pending <= 0.0 {
        return Vec::new();
    }
    let seen_once = !history && !grants;
    // Without Query Store's per-database waits, the totals are the instance's since it started: every database's.
    let shared = instance && !windowed;
    vec![MemoryQueue {
        share,
        memory_per_hour: memory_per_hour.unwrap_or(0.0),
        history,
        windowed,
        shared,
        grant_ms,
        grants_per_hour: grants_per_hour.unwrap_or(0.0),
        grants,
        pending,
        grant_story: grants || pending > 0.0,
        seen_once,
        // A measured window can say High; totals since startup describe the past and say at most Medium; one
        // sighting with nothing on record is Low. The instance's totals, with nothing of this database's queueing,
        // don't say this database is the one waiting: Low.
        severity: if (seen_once && pending < 5.0) || (shared && pending <= 0.0) {
            Severity::Low
        } else if history && windowed && share >= 0.2 && memory_per_hour.is_some_and(|m| m >= 10.0 * MINUTE_MS) {
            Severity::High
        } else {
            Severity::Medium
        },
        confidence: if seen_once {
            Confidence::Low
        } else if pending > 0.0 || (history && windowed) {
            Confidence::High
        } else {
            Confidence::Medium
        },
    }]
}

fn memory_queue_finding(snapshot: &Snapshot, m: MemoryQueue) -> Finding {
    let mut f = finding(
        "memory-queue",
        &server_key(snapshot),
        Category::Configuration,
        subject(SubjectKind::Database, snapshot.server.database.clone()),
    );
    let (share, per_hour) = (format::percent(m.share), format::duration(m.memory_per_hour));
    let pending = format::count(m.pending);
    // The instance's totals (`shared`) are only said with queries queueing now, whose title it is.
    f.title = if m.pending > 0.0 {
        t!("check.memory_queue.title.pending", n = m.pending, count = &pending)
    } else if m.history {
        t!("check.memory_queue.title.history", share = &share, per_hour = &per_hour)
    } else {
        t!("check.memory_queue.title.grants", time = format::duration(m.grant_ms), since = uptime_phrase(snapshot))
    };
    let mut parts = Vec::new();
    if m.history {
        let window = if m.windowed {
            snapshot.workload.as_ref().map(|w| w.window_phrase()).unwrap_or_default()
        } else {
            t!("check.memory_queue.window.since_started", since = uptime_phrase(snapshot))
        };
        parts.push(if m.shared {
            t!("check.memory_queue.summary.history_instance", share = &share, window = window, per_hour = &per_hour)
        } else {
            t!("check.memory_queue.summary.history", share = &share, window = window, per_hour = &per_hour)
        });
    }
    // Grant waits on record are this database's (on Azure SQL Database): an instance's are never said as its.
    if m.grants {
        parts.push(t!(
            "check.memory_queue.summary.grants",
            since = uptime_opening(snapshot),
            time = format::duration(m.grant_ms),
            per_hour = format::duration(m.grants_per_hour)
        ));
    }
    if m.pending > 0.0 {
        parts.push(t!("check.memory_queue.summary.pending", n = m.pending, count = &pending));
    }
    parts.push(if m.grant_story {
        t!("check.memory_queue.summary.grant_story")
    } else {
        t!("check.memory_queue.summary.allocations")
    });
    f.summary = parts.join(" ");
    f.severity = m.severity;
    f.impact.score = base_score(f.severity) + if m.history { m.share.min(1.0) * 40.0 } else { 0.0 };
    f.impact.label = if m.history {
        t!("check.memory_queue.impact.share", share = &share)
    } else if m.pending > 0.0 {
        t!("check.memory_queue.impact.pending", count = &pending)
    } else {
        t!("check.memory_queue.impact.grants", time = format::duration(m.grant_ms))
    };
    f.confidence = m.confidence;
    if m.seen_once {
        f.caveats.push(seen_once());
    }
    if m.history {
        f.metrics.push(Metric::new(t!("check.memory_queue.metric.share"), m.share, Unit::Ratio));
        f.metrics.push(Metric::new(t!("check.memory_queue.metric.per_hour"), m.memory_per_hour, Unit::Ms));
    }
    if m.grants {
        f.metrics.push(Metric::new(t!("check.memory_queue.metric.grants"), m.grant_ms, Unit::Ms));
    }
    if m.pending > 0.0 {
        f.metrics.push(Metric::new(t!("check.memory_queue.metric.pending"), m.pending, Unit::Count));
    }
    f.fix = Some(Fix {
        summary: if m.grant_story { t!("check.memory_queue.fix.grants") } else { t!("check.memory_queue.fix.sample") },
        sql: None,
        cost: None,
    });
    f.verify = vec![t!("check.memory_queue.verify")];
    f
}
