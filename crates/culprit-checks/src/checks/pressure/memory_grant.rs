//! The `memory-grant` check: Statements whose memory grant (reserved before they run, for sorts and hashes) is many times what they use, so other queries queue for memory.

use super::*;
use crate::evidence::{QueryMeasure, Requirement as Evidence};

pub fn memory_grant_info() -> CheckInfo {
    CheckInfo {
        id: "memory-grant".into(),
        title: t!("check.memory_grant.info.title"),
        category: Category::Queries,
        engines: vec![Engine::Mssql],
        requires: vec![],
        description: t!("check.memory_grant.info.description"),
    }
}

check!(
    MemoryGrantCheck: memory_grant_info,
    decide = oversized_grants,
    render = memory_grant_finding,
    case<'s> = OversizedGrant<'s>,
    evidence = &[Evidence::Query(QueryMeasure::Grants)],
);

/// The memory-grant findings of `snapshot`: its cases, each worded.
#[cfg(test)]
pub(crate) fn memory_grant(snapshot: &Snapshot) -> Vec<Finding> {
    culprit_core::Check::run(&MemoryGrantCheck, snapshot)
}

/// A statement whose memory grant is many times what its runs use.
pub struct OversizedGrant<'s> {
    grant: &'s GrantStat,
    /// The statement, when the workload has it.
    query: Option<&'s QueryStat>,
    /// It stopped running: told in the past.
    stopped: bool,
    /// What the grant costs the statement: its own time waiting for memory, when it waited.
    impact: Option<QueryImpact>,
    severity: Severity,
    /// Its rank among the grants: the larger of its share's score and its severity's.
    score: f64,
}

/// The five over-sized grants that cost the most.
fn oversized_grants(snapshot: &Snapshot) -> Vec<OversizedGrant<'_>> {
    let Some(p) = pressure(snapshot) else { return Vec::new() };
    // Queueing is on record (grant waits of a minute an hour or more), or several queries were queueing when the scan
    // ran; one sighting is what memory-queue calls Low, not grounds for High here.
    let pending = signal(snapshot, "memory-grants-pending").unwrap_or(0.0);
    let grant_waits = counter_hours(snapshot).map(|h| signal(snapshot, "grant-wait-ms").unwrap_or(0.0) / h);
    let queueing = pending >= 5.0 || grant_waits.is_some_and(|g| g >= MINUTE_MS);
    // Recent runs that use a fair part of their grant (memory grant feedback shrank it) are fixed already.
    let still_oversized = |g: &&GrantStat| match (g.last_granted_kb, g.last_used_kb) {
        (Some(granted), Some(used)) if granted >= 0.0 => granted >= 50.0 * MB && used <= granted * 0.25,
        _ => true,
    };
    let mut grants: Vec<OversizedGrant<'_>> = p
        .grants
        .iter()
        .filter(|g| g.executions >= 5.0 && g.avg_granted_kb >= 50.0 * MB && g.avg_used_kb <= g.avg_granted_kb * 0.25)
        .filter(still_oversized)
        .map(|grant| {
            let query = by_hash(snapshot, &grant.query_hash);
            // What the grant costs this query is its own time waiting for memory, not all of its time: the rest is
            // running, which a smaller grant doesn't change.
            let impact = query.and_then(|q| {
                let waited: f64 =
                    q.waits.iter().filter(|w| category_key(&w.category) == "memory").map(|w| w.wait_ms).sum();
                let w = snapshot.workload.as_ref().filter(|_| waited > 0.0)?;
                // A day's worth only of runs that came that way, never a burst's spread over the window.
                let per_day_ms = w.per_day_measured(waited).filter(|_| w.runs_per_day_of(q).is_some());
                Some(QueryImpact { share: w.share_of(waited, q), per_day_ms })
            });
            let wasted_gb = (grant.avg_granted_kb - grant.avg_used_kb) / MB / MB;
            let severity = if queueing && wasted_gb >= 1.0 {
                Severity::High
            } else if queueing || wasted_gb >= 0.5 {
                Severity::Medium
            } else {
                Severity::Low
            };
            OversizedGrant {
                grant,
                query,
                stopped: query.is_some_and(|q| crate::all_stopped(snapshot, &[q])),
                score: impact.as_ref().map_or(0.0, |i| score_for_share(i.share)).max(base_score(severity)),
                impact,
                severity,
            }
        })
        .collect();
    grants.sort_by(|a, b| b.score.total_cmp(&a.score));
    grants.truncate(5);
    grants
}

fn memory_grant_finding(_: &Snapshot, o: OversizedGrant<'_>) -> Finding {
    let g = o.grant;
    let key = o.query.map(|q| q.id.clone()).unwrap_or_else(|| g.query_hash.clone());
    let mut f = finding("memory-grant", &key, Category::Queries, subject(SubjectKind::Query, &key));
    let (granted, used) = (kb(g.avg_granted_kb), kb(g.avg_used_kb));
    f.title = match o.query {
        Some(q) if o.stopped => t!(
            "check.memory_grant.title.stopped",
            query = capitalize(&query_handle(q)),
            granted = &granted,
            used = &used
        ),
        Some(q) => t!(
            "check.memory_grant.title.running",
            query = capitalize(&query_handle(q)),
            granted = &granted,
            used = &used
        ),
        None => t!("check.memory_grant.title.unknown", granted = &granted, used = &used),
    };
    let share_used = if g.avg_granted_kb > 0.0 { g.avg_used_kb / g.avg_granted_kb } else { 0.0 };
    f.summary = t!(
        "check.memory_grant.summary",
        granted = &granted,
        used = &used,
        share = format::percent(share_used),
        runs = format::count(g.executions),
        largest = kb(g.max_granted_kb)
    );
    if let Some(q) = o.query {
        f.queries = query_refs(&[q]);
    }
    if let Some(impact) = &o.impact {
        apply_query_impact(&mut f, impact);
    }
    f.severity = o.severity;
    f.impact.score = o.score;
    f.impact.label = t!("check.memory_grant.impact", granted = &granted, used = &used);
    f.confidence = Confidence::High;
    f.metrics = vec![
        Metric::new(t!("check.memory_grant.metric.reserved"), g.avg_granted_kb * 1024.0, Unit::Bytes),
        Metric::new(t!("check.memory_grant.metric.used"), g.avg_used_kb * 1024.0, Unit::Bytes),
        Metric::new(t!("check.memory_grant.metric.runs"), g.executions, Unit::Count),
    ];
    f.fix = Some(Fix {
        summary: t!("check.memory_grant.fix"),
        sql: Some("-- Stopgap, appended to the statement: limit its grant to 5% of query memory\nOPTION (MAX_GRANT_PERCENT = 5);".into()),
        cost: None,
    });
    f.verify = vec![t!("check.memory_grant.verify")];
    f
}
