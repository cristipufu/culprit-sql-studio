//! Material recorded network waits without exclusive elapsed-time or slow-client inference.

use super::*;
use crate::evidence::{QueryMeasure, Requirement as Evidence};
use culprit_core::t;
use culprit_core::telemetry::WaitClass;

pub fn network_wait_info() -> CheckInfo {
    CheckInfo {
        id: "network-wait".into(),
        title: t!("check.network_wait.info.title"),
        category: Category::Queries,
        engines: Engine::ALL.to_vec(),
        requires: vec![],
        description: t!("check.network_wait.info.description"),
    }
}

check!(
    NetworkWaitCheck: network_wait_info,
    decide = network_waits,
    render = network_wait_finding,
    case<'s> = NetworkWait<'s>,
    evidence = &[Evidence::Query(QueryMeasure::Waits(WaitClass::Network))],
);

/// The network-wait findings of `snapshot`: its cases, each worded.
#[cfg(test)]
pub(crate) fn network_wait(snapshot: &Snapshot) -> Vec<Finding> {
    culprit_core::Check::run(&NetworkWaitCheck, snapshot)
}

/// A material observed network-wait total, with no exclusive elapsed-time or client-cause inference.
pub struct NetworkWait<'s> {
    query: &'s QueryStat,
    recorded_ms: f64,
    rows_per_execution: Option<f64>,
    ranking_score: f64,
    stopped: bool,
    complete: bool,
    severity: Severity,
}

/// Selection policies only; neither ratio is exported as a wait share of elapsed time.
const MATERIAL_NETWORK_RUNTIME_RATIO: f64 = 0.25;
const MIN_QUERY_WORKLOAD_SHARE: f64 = 0.01;
const LARGE_RECORDED_ROWS: f64 = 1_000.0;
const MAX_NETWORK_FINDINGS: usize = 5;

fn network_waits(snapshot: &Snapshot) -> Vec<NetworkWait<'_>> {
    let Some(workload) = snapshot.workload.as_ref() else { return Vec::new() };
    let total_runtime = workload.total_time_ms;
    let mut cases: Vec<_> = app_queries(snapshot)
        .filter_map(|query| {
            let recorded_ms = query.observed_wait_ms(WaitClass::Network).filter(|ms| *ms > 0.0)?;
            if !query.total_time_ms.is_finite()
                || query.total_time_ms <= 0.0
                || recorded_ms < query.total_time_ms * MATERIAL_NETWORK_RUNTIME_RATIO
                || query_impact(snapshot, &[query])?.share < MIN_QUERY_WORKLOAD_SHARE
            {
                return None;
            }
            let rows_per_execution = query
                .rows
                .filter(|rows| rows.is_finite() && *rows >= 0.0)
                .filter(|_| query.executions.is_finite() && query.executions > 0.0)
                .map(|rows| rows / query.executions)
                .filter(|rows| rows.is_finite());
            let comparison = (total_runtime.is_finite() && total_runtime > 0.0)
                .then(|| recorded_ms / total_runtime)
                .filter(|ratio| ratio.is_finite())
                .unwrap_or(0.0);
            let severity = rate_per_day(snapshot, recorded_ms)
                .filter(|rate| rate.is_finite() && *rate >= 0.0)
                .map(|_| severity_for_time(snapshot, recorded_ms, comparison))
                .unwrap_or(Severity::Low);
            Some(NetworkWait {
                query,
                recorded_ms,
                rows_per_execution,
                ranking_score: score_for_share(comparison).max(base_score(severity)),
                severity,
                stopped: crate::all_stopped(snapshot, &[query]),
                complete: query.measured_wait_ms(WaitClass::Network).is_some(),
            })
        })
        .collect();
    cases.sort_by(|a, b| b.ranking_score.total_cmp(&a.ranking_score));
    cases.truncate(MAX_NETWORK_FINDINGS);
    cases
}

fn network_wait_finding(snapshot: &Snapshot, wait: NetworkWait<'_>) -> Finding {
    let query = wait.query;
    let mut finding = finding("network-wait", &query.id, Category::Queries, subject(SubjectKind::Query, &query.id));
    let handle = capitalize(&query_handle(query));
    let recorded = format::duration(wait.recorded_ms);
    finding.title = match (wait.rows_per_execution.filter(|rows| *rows >= LARGE_RECORDED_ROWS), wait.stopped) {
        (Some(rows), false) => {
            t!("check.network_wait.title.rows", query = handle, time = &recorded, rows = format::count(rows))
        }
        (Some(rows), true) => {
            t!("check.network_wait.title.rows_stopped", query = handle, time = &recorded, rows = format::count(rows))
        }
        (None, false) => t!("check.network_wait.title.waits", query = handle, time = &recorded),
        (None, true) => t!("check.network_wait.title.waited", query = handle, time = &recorded),
    };
    finding.summary = t!(
        "check.network_wait.summary",
        time = &recorded,
        period = snapshot.workload.as_ref().map(|workload| workload.window_phrase()).unwrap_or_default()
    );
    finding.severity = wait.severity;
    finding.confidence = Confidence::High;
    finding.queries = query_refs(&[query]);
    finding.last_seen = query.last_seen.clone();
    finding.impact.score = wait.ranking_score;
    finding.impact.set_label(t!("check.network_wait.impact", time = &recorded));
    finding.metrics.push(Metric::said(
        culprit_core::key!("check.network_wait.metric.recorded"),
        wait.recorded_ms,
        Unit::Ms,
    ));
    if let Some(rows) = wait.rows_per_execution {
        finding.metrics.push(Metric::said(culprit_core::key!("check.network_wait.metric.rows"), rows, Unit::Rows));
    }
    if let Some(period) = totals_basis(snapshot, query) {
        for metric in &mut finding.metrics {
            metric.label = t!("common.metric.over", label = &metric.label, span = &period);
        }
    }
    if !wait.complete {
        finding.caveats.push(t!("check.network_wait.caveat.partial"));
    }
    finding.fix = Some(Fix { summary: t!("check.network_wait.fix"), sql: None, cost: None });
    finding.verify = vec![t!("check.network_wait.verify")];
    finding
}

#[cfg(test)]
mod tests;
