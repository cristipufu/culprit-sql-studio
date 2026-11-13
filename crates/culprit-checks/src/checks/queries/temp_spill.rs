//! Recorded temporary resource work is useful query context. It does not prove spill duration or memory exhaustion.
use super::*;
use crate::{
    evidence::{QueryMeasure, Requirement as Evidence},
    views::temporary_work::{self, TemporaryWorkEvidence},
};
use culprit_core::{Actionability, StatementTemporaryWork, t};

// Context selection only: this does not assign any part of the query's time to temporary work.
const MIN_QUERY_CONTEXT_SHARE: f64 = 0.01;

pub fn temp_spill_info() -> CheckInfo {
    CheckInfo {
        id: "temp-spill".into(),
        title: t!("check.temp_spill.info.title"),
        category: Category::Queries,
        engines: Engine::ALL.to_vec(),
        requires: vec![],
        description: t!("check.temp_spill.info.description"),
    }
}

check!(
    TempSpillCheck: temp_spill_info,
    decide = temporary_work,
    render = render,
    case<'s> = TemporaryWork<'s>,
    evidence = &[Evidence::Query(QueryMeasure::TemporaryWork)],
);

pub struct TemporaryWork<'a> {
    query: &'a QueryStat,
    evidence: TemporaryWorkEvidence<'a>,
    stopped: bool,
}

fn temporary_work(snapshot: &Snapshot) -> Vec<TemporaryWork<'_>> {
    app_queries(snapshot)
        .filter_map(|query| {
            let work = query.temporary_work.as_ref()?;
            if !relevant_work(work) || query_impact(snapshot, &[query])?.share < MIN_QUERY_CONTEXT_SHARE {
                return None;
            }
            Some(TemporaryWork {
                query,
                evidence: temporary_work::evidence(work)?,
                stopped: crate::all_stopped(snapshot, &[query]),
            })
        })
        .collect()
}

fn relevant_work(work: &StatementTemporaryWork) -> bool {
    match work {
        StatementTemporaryWork::TableCreation { disk_tables, sort_merge_passes, .. } => {
            [disk_tables, sort_merge_passes]
                .into_iter()
                .flatten()
                .any(|value| value.parse::<u64>().is_ok_and(|value| value > 0))
        }
        _ => work.has_observed_work(),
    }
}

fn render(snapshot: &Snapshot, case: TemporaryWork<'_>) -> Finding {
    let observed = temporary_work::explain(case.evidence);
    let query = case.query;
    let mut finding = finding("temp-spill", &query.id, Category::Queries, subject(SubjectKind::Query, &query.id));
    finding.title = t!("check.temporary_work.title", query = capitalize(&query_handle(query)), work = observed.title);
    finding.summary = observed.summary;
    finding.severity = Severity::Low;
    finding.confidence = Confidence::High;
    finding.actionability = if case.stopped { Actionability::History } else { Actionability::Information };
    // Native counters are independent observations. Do not assign query elapsed time or estimated savings to them.
    finding.impact.score = base_score(Severity::Low);
    finding.queries = vec![QueryRef { id: query.id.clone(), plan_node: None, note: None }];
    finding.metrics = observed.metrics;
    finding.caveats = observed.limitations;
    if let Some(basis) = crate::support::totals_basis(snapshot, query) {
        finding.caveats.push(t!("check.temporary_work.totals_basis", basis = basis));
    }
    finding.fix = Some(Fix { summary: observed.investigation, sql: None, cost: None });
    finding.verify = vec![observed.verify];
    finding
}
