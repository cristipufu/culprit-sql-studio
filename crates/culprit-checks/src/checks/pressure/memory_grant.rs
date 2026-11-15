//! A retained cache entry can show grant/use imbalance without identifying a current memory-pressure cause.
use super::*;
use crate::{
    evidence::Requirement as Evidence,
    views::statement_grants::{self, GrantCase, GrantUse},
};
use culprit_core::{Actionability, StatementGrantReference, VerificationBasis, WorkloadDependency, source_id};
use std::collections::BTreeSet;

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
    decide = recorded_imbalances,
    render = memory_grant_finding,
    case<'s> = RecordedImbalance<'s>,
    evidence = &[Evidence::Source(source_id::GRANTS), Evidence::Incomplete(key!("evidence.statement_grants_historical"))],
);

#[cfg(test)]
pub(crate) fn memory_grant(snapshot: &Snapshot) -> Vec<Finding> {
    culprit_core::Check::run(&MemoryGrantCheck, snapshot)
}

pub struct RecordedImbalance<'s> {
    grant: GrantCase<'s>,
    recorded_difference_kibibytes: f64,
}

const MAX_RECORDED_IMBALANCES: usize = 5;

fn recorded_imbalances(snapshot: &Snapshot) -> Vec<RecordedImbalance<'_>> {
    let Some(pressure) = pressure(snapshot) else { return Vec::new() };
    let mut cases: Vec<_> = pressure
        .grants
        .iter()
        .filter_map(|read| statement_grants::read_cases(read, true).ok())
        .flatten()
        .filter(|case| !case.row.context.origin.is_culprit())
        .filter_map(|grant| match grant.usage {
            GrantUse::RecordedImbalance(averages) => Some(RecordedImbalance {
                recorded_difference_kibibytes: averages.granted_kibibytes - averages.used_kibibytes,
                grant,
            }),
            _ => None,
        })
        .collect();
    // Rank individual recorded differences, never sum overlapping reads or charge them as elapsed-time impact.
    cases.sort_by(|left, right| {
        right.recorded_difference_kibibytes.total_cmp(&left.recorded_difference_kibibytes).then_with(|| {
            (&left.grant.entry_key, &left.grant.snapshot_key).cmp(&(&right.grant.entry_key, &right.grant.snapshot_key))
        })
    });
    let mut entries = BTreeSet::new();
    cases.retain(|case| entries.insert(case.grant.entry_key.clone()));
    cases.truncate(MAX_RECORDED_IMBALANCES);
    cases
}

fn memory_grant_finding(snapshot: &Snapshot, case: RecordedImbalance<'_>) -> Finding {
    let key = case.grant.entry_key.clone();
    let source = snapshot.source_outcome(source_id::GRANTS).map(std::slice::from_ref).unwrap_or(&[]);
    let caveats = statement_grants::limitations(std::slice::from_ref(case.grant.snapshot), source);
    let reference = StatementGrantReference {
        snapshot_key: case.grant.snapshot_key.clone(),
        observation_key: case.grant.observation_key.clone(),
        capture: case.grant.snapshot.capture.clone(),
        identity: case.grant.row.context.identity.clone(),
    };
    let entry = statement_grants::render(case.grant);
    let mut finding = finding("memory-grant", &key, Category::Queries, subject(SubjectKind::Query, &key));
    finding.title = entry.title.render();
    finding.summary = entry.explanation.render();
    finding.actionability = Actionability::Information;
    finding.confidence = Confidence::High;
    finding.impact.score = base_score(Severity::Low);
    finding
        .impact
        .set_keyed_label(key!("check.memory_grant.impact.recorded"), t!("check.memory_grant.impact.recorded"));
    finding.metrics = entry.metrics;
    finding.fix = Some(Fix { summary: entry.investigation.render(), sql: None, cost: None });
    finding.verify = vec![entry.verify.render()];
    finding.verification_basis = VerificationBasis::CounterChange;
    finding.caveats = caveats.iter().map(culprit_core::i18n::Message::render).collect();
    finding.facts.workload_dependency = Some(WorkloadDependency::Independent);
    finding.facts.statement_grant = Some(reference);
    finding
}

#[cfg(test)]
mod scope_tests;
