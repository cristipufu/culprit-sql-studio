//! Material recorded lock waits without exclusive elapsed-time or ownership inference.

use super::*;
use crate::evidence::{QueryMeasure, Requirement as Evidence};
use culprit_core::telemetry::WaitClass;

pub fn lock_time_info() -> CheckInfo {
    CheckInfo {
        id: "lock-time".into(),
        title: t!("check.lock_time.info.title"),
        category: Category::Queries,
        engines: Engine::ALL.to_vec(),
        requires: vec![],
        description: t!("check.lock_time.info.description"),
    }
}

check!(
    LockTimeCheck: lock_time_info,
    decide = lock_waits,
    render = lock_time_finding,
    case<'s> = LockWait<'s>,
    evidence = &[Evidence::Query(QueryMeasure::Waits(WaitClass::Lock))],
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

/// Material positive lock-wait evidence, without exclusive elapsed-time or ownership assumptions.
pub struct LockWait<'s> {
    query: &'s QueryStat,
    recorded_ms: f64,
    per_execution_ms: Option<f64>,
    severity: Severity,
    ranking_score: f64,
    no_rate: bool,
    complete: bool,
    versioning_candidate: bool,
}

const MIN_RECORDED_LOCK_MS: f64 = 10_000.0;
/// Selection policy only: accumulated worker waits are not an exclusive share of elapsed time.
const MATERIAL_LOCK_RUNTIME_RATIO: f64 = 0.25;
const MAX_LOCK_FINDINGS: usize = 5;

fn lock_waits(snapshot: &Snapshot) -> Vec<LockWait<'_>> {
    let total_runtime = snapshot
        .workload
        .as_ref()
        .map(|workload| workload.total_time_ms)
        .filter(|total| total.is_finite() && *total > 0.0);
    let mut cases: Vec<_> = app_queries(snapshot)
        // Lock timeouts already have the failing-query finding.
        .filter(|query| !times_out_on_locks(query))
        .filter(|query| !lock_time_counts_the_run(snapshot, query))
        .filter_map(|query| {
            let recorded_ms = query.observed_wait_ms(WaitClass::Lock)?;
            if recorded_ms < MIN_RECORDED_LOCK_MS
                || (query.total_time_ms.is_finite()
                    && query.total_time_ms > 0.0
                    && recorded_ms < query.total_time_ms * MATERIAL_LOCK_RUNTIME_RATIO)
            {
                return None;
            }
            // Native totals are rated over their native window, never projected onto current scan cadence.
            let daily_ms = rate_per_day(snapshot, recorded_ms).filter(|rate| rate.is_finite() && *rate >= 0.0);
            let comparison =
                total_runtime.map(|total| recorded_ms / total).filter(|ratio| ratio.is_finite()).unwrap_or(0.0);
            let by_amount = since_start_severity(daily_ms, LOCK_HIGH_MS_PER_DAY, LOCK_MEDIUM_MS_PER_DAY);
            let severity = severity_for_share(comparison).min(by_amount);
            let per_execution_ms = (query.executions.is_finite() && query.executions > 0.0)
                .then(|| recorded_ms / query.executions)
                .filter(|mean| mean.is_finite());
            Some(LockWait {
                query,
                recorded_ms,
                per_execution_ms,
                severity,
                ranking_score: score_for_share(comparison).max(base_score(severity)),
                no_rate: daily_ms.is_none(),
                complete: query.measured_wait_ms(WaitClass::Lock).is_some(),
                versioning_candidate: crate::config::read_committed_snapshot_off(snapshot)
                    && crate::config::row_versioning_candidate(query),
            })
        })
        .collect();
    cases.sort_by(|a, b| b.ranking_score.total_cmp(&a.ranking_score));
    cases.truncate(MAX_LOCK_FINDINGS);
    cases
}

fn lock_time_finding(snapshot: &Snapshot, wait: LockWait<'_>) -> Finding {
    let query = wait.query;
    let mut finding = finding("lock-time", &query.id, Category::Queries, subject(SubjectKind::Query, &query.id));
    let recorded = format::duration(wait.recorded_ms);
    finding.title = t!(
        "check.lock_time.title.recorded",
        handle = capitalize(&query_handle(query)),
        tense = tense(snapshot, &[query]),
        lock = &recorded
    );
    finding.summary = t!(
        "check.lock_time.summary.waited",
        lock = &recorded,
        period = snapshot.workload.as_ref().map(|workload| workload.window_phrase()).unwrap_or_default()
    );
    finding.severity = wait.severity;
    finding.confidence = Confidence::High;
    finding.impact.score = wait.ranking_score;
    finding.impact.set_label(t!("check.lock_time.impact", lock = &recorded));
    finding.queries = query_refs(&[query]);
    finding.last_seen = query.last_seen.clone();
    finding.metrics.push(Metric::said(
        culprit_core::key!("check.lock_time.metric.lock_wait"),
        wait.recorded_ms,
        Unit::Ms,
    ));
    if let Some(mean) = wait.per_execution_ms {
        finding.metrics.push(Metric::said(culprit_core::key!("check.lock_time.metric.per_run"), mean, Unit::Ms));
    }
    if let Some(period) = totals_basis(snapshot, query) {
        for metric in &mut finding.metrics {
            metric.label = t!("common.metric.over", label = &metric.label, span = &period);
        }
    }
    if wait.no_rate {
        finding.caveats.push(t!("check.lock_time.caveat.no_rate"));
    }
    if !wait.complete {
        finding.caveats.push(t!("check.lock_time.caveat.partial"));
    }
    finding.facts.lock_wait =
        Some(culprit_core::FindingLockWait { ms: wait.recorded_ms, versioning_candidate: wait.versioning_candidate });
    // Catch a representative wait before assigning a resource, blocker or corrective change.
    let LockHolder { lead, sql } = find_lock_holder(snapshot, query, None);
    finding.fix = Some(Fix {
        summary: match (wait.versioning_candidate, catches_live(snapshot, query, None)) {
            (true, true) => t!("check.lock_time.fix.versioning_live"),
            (true, false) => t!("check.lock_time.fix.versioning", lead = &lead),
            (false, _) => t!("check.lock_time.fix.holders", lead = &lead),
        },
        sql,
        cost: None,
    });
    finding.facts.advice.running_totals_explained = crate::queries::running_totals(snapshot).is_some();
    finding.verify = vec![match crate::queries::running_totals(snapshot) {
        Some(source) => t!(
            "check.lock_time.verify.between_scans",
            source = source,
            now = t!("check.lock_time.verify.now", lock = &recorded)
        ),
        None => t!("check.lock_time.verify.rescan"),
    }];
    finding
}

#[cfg(test)]
mod tests;
