//! The `network-wait` check: Queries that spend much of their time waiting for the application to take results (ASYNC_NETWORK_IO), usually because they return large result sets.

use super::*;
use crate::evidence::{QueryMeasure, Requirement as Evidence};
use culprit_core::t;

pub fn network_wait_info() -> CheckInfo {
    CheckInfo {
        id: "network-wait".into(),
        title: t!("check.network_wait.info.title"),
        category: Category::Queries,
        engines: vec![Engine::Mssql],
        requires: vec![],
        description: t!("check.network_wait.info.description"),
    }
}

check!(
    NetworkWaitCheck: network_wait_info,
    decide = network_waits,
    render = network_wait_finding,
    case<'s> = NetworkWait<'s>,
    evidence = &[Evidence::Query(QueryMeasure::Waits("Network"))],
);

/// The network-wait findings of `snapshot`: its cases, each worded.
#[cfg(test)]
pub(crate) fn network_wait(snapshot: &Snapshot) -> Vec<Finding> {
    culprit_core::Check::run(&NetworkWaitCheck, snapshot)
}

/// A statement that spends a quarter or more of its time waiting for the application to take its results.
pub struct NetworkWait<'s> {
    query: &'s QueryStat,
    /// The share of its time spent waiting on the application.
    share: f64,
    /// Rows each run returns.
    rows: Option<f64>,
    /// What waiting on the application costs: the time waiting, not the query's whole time.
    impact: QueryImpact,
    /// It stopped running: told in the past.
    stopped: bool,
    severity: Severity,
}

/// The five statements whose waiting on the application costs the most.
fn network_waits(snapshot: &Snapshot) -> Vec<NetworkWait<'_>> {
    let mut cases: Vec<NetworkWait<'_>> = app_queries(snapshot)
        .filter_map(|q| {
            let network: f64 =
                q.waits.iter().filter(|w| category_key(&w.category) == "network").map(|w| w.wait_ms).sum();
            let share = if q.total_time_ms > 0.0 { network / q.total_time_ms } else { 0.0 };
            let query = query_impact(snapshot, &[q])?;
            if share < 0.25 || query.share < 0.01 {
                return None;
            }
            let rows = q.rows.filter(|_| q.executions > 0.0).map(|r| r / q.executions);
            // Its weight is the time spent waiting on the application, not the query's whole time.
            let w = snapshot.workload.as_ref()?;
            // A day's worth only of runs that came that way, never a burst's spread over the window.
            let per_day_ms = w.per_day_measured(network).filter(|_| w.runs_per_day_of(q).is_some());
            let impact = QueryImpact { share: w.share_of(network, q), per_day_ms };
            Some(NetworkWait {
                query: q,
                share,
                rows,
                severity: severity_for_share_of(snapshot, &[q], impact.share),
                impact,
                // A statement that stopped running is told in the past.
                stopped: crate::all_stopped(snapshot, &[q]),
            })
        })
        .collect();
    cases.sort_by(|a, b| score_for_share(b.impact.share).total_cmp(&score_for_share(a.impact.share)));
    cases.truncate(5);
    cases
}

fn network_wait_finding(_: &Snapshot, n: NetworkWait<'_>) -> Finding {
    let q = n.query;
    let mut f = finding("network-wait", &q.id, Category::Queries, subject(SubjectKind::Query, &q.id));
    apply_query_impact(&mut f, &n.impact);
    let (query, share) = (capitalize(&query_handle(q)), format::percent(n.share));
    f.title = match (n.rows.filter(|r| *r >= 1_000.0), n.stopped) {
        (Some(r), false) => t!("check.network_wait.title.rows", query = query, share = share, rows = format::count(r)),
        (Some(r), true) => {
            t!("check.network_wait.title.rows_stopped", query = query, share = share, rows = format::count(r))
        }
        (None, false) => t!("check.network_wait.title.waits", query = query, share = share),
        (None, true) => t!("check.network_wait.title.waited", query = query, share = share),
    };
    f.summary = t!("check.network_wait.summary", share = format::percent(n.share));
    f.severity = n.severity;
    f.confidence = Confidence::High;
    f.queries = query_refs(&[q]);
    f.metrics.push(Metric::said(culprit_core::key!("check.network_wait.metric.share"), n.share, Unit::Ratio));
    f.facts.network_wait_share = Some(n.share);
    if let Some(r) = n.rows {
        f.metrics.push(Metric::said(culprit_core::key!("check.network_wait.metric.rows"), r, Unit::Rows));
    }
    f.fix = Some(Fix { summary: t!("check.network_wait.fix"), sql: None, cost: None });
    f.verify = vec![t!("check.network_wait.verify", results = culprit_core::t!("core.waits.category.network.label"))];
    f
}
