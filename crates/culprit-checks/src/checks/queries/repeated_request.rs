//! The `repeated-request` check: statements a structured log correlates with requests that send them many times.

use super::*;
use crate::evidence::{Observation, Requirement as Evidence};
use culprit_core::operations::RequestEvidence;
use culprit_core::t;

pub fn repeated_request_info() -> CheckInfo {
    CheckInfo {
        id: "repeated-request".into(),
        title: t!("check.repeated_request.info.title"),
        category: Category::Queries,
        engines: vec![Engine::Mssql, Engine::Postgres, Engine::Yugabyte, Engine::Mysql, Engine::Sqlite],
        requires: vec![Capability::QueryLog],
        description: t!("check.repeated_request.info.description"),
    }
}

check!(
    RepeatedRequestCheck: repeated_request_info,
    decide = repeated_requests,
    render = repeated_request_finding,
    case<'s> = RepeatedRequest<'s>,
    evidence = &[Evidence::Observed(Observation::RequestCounts, &[])],
);

/// A statement that repeats within the requests a log correlates it with: three or more requests send it ten times
/// or more.
pub struct RepeatedRequest<'s> {
    query: &'s QueryStat,
    requests: &'s RequestEvidence,
}

fn repeated_requests(s: &Snapshot) -> Vec<RepeatedRequest<'_>> {
    s.queries()
        .iter()
        .filter_map(|query| {
            let requests = query.requests.as_ref()?;
            (requests.repeated_requests >= 3 && requests.most_calls >= 10)
                .then_some(RepeatedRequest { query, requests })
        })
        .collect()
}

fn repeated_request_finding(_: &Snapshot, repeated: RepeatedRequest<'_>) -> Finding {
    let RepeatedRequest { query: q, requests: r } = repeated;
    let mut f = finding("repeated-request", &q.id, Category::Queries, subject(SubjectKind::Query, &q.id));
    // Counts as the log reader gives them, every digit.
    let repeated = r.repeated_requests.to_string();
    f.title = t!("check.repeated_request.title", requests = repeated.clone());
    let mut summary = vec![
        t!(
            "check.repeated_request.summary.correlated",
            requests = r.requests.to_string(),
            calls = r.most_calls.to_string()
        ),
        t!("check.repeated_request.summary.not_proof"),
    ];
    if r.sample_truncated {
        summary.push(t!("check.repeated_request.summary.truncated"));
    }
    summary.push(t!("check.repeated_request.summary.hashed"));
    f.summary = summary.join(" ");
    f.severity = Severity::Low;
    f.confidence = Confidence::Medium;
    f.impact.score = base_score(Severity::Low);
    f.impact.set_label(t!("check.repeated_request.impact", requests = repeated));
    f.queries = query_refs(&[q]);
    f.metrics = vec![
        Metric::said(culprit_core::key!("check.repeated_request.metric.requests"), r.requests as f64, Unit::Count),
        Metric::said(
            culprit_core::key!("check.repeated_request.metric.repeated"),
            r.repeated_requests as f64,
            Unit::Count,
        ),
        Metric::said(culprit_core::key!("check.repeated_request.metric.most_calls"), r.most_calls as f64, Unit::Count),
    ];
    f.fix = Some(Fix { summary: t!("check.repeated_request.fix"), sql: None, cost: None });
    f.verify = vec![t!("check.repeated_request.verify")];
    f
}
