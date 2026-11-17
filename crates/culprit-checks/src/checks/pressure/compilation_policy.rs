//! Exact retained compilation work and an explicit statement directive establish a policy to test, not live pressure.
use super::*;
use crate::{evidence::Requirement as Evidence, views::compilation};
use culprit_core::{Actionability, VerificationBasis, WorkloadDependency, source_id};

const MAX_POLICY_FINDINGS: usize = 5;

pub fn compilation_policy_info() -> CheckInfo {
    CheckInfo {
        id: "compilation-policy".into(),
        title: t!("check.compilation_policy.info.title"),
        category: Category::Queries,
        engines: vec![Engine::Mssql],
        requires: vec![],
        description: t!("check.compilation_policy.info.description"),
    }
}

check!(
    CompilationPolicyCheck: compilation_policy_info,
    decide = recorded_recompilations,
    render = compilation_policy_finding,
    case<'s> = RecordedRecompilation<'s>,
    evidence = &[Evidence::Source(source_id::QUERY_COMPILATION), Evidence::Incomplete(key!("evidence.compilation_policy_historical"))],
);

pub struct RecordedRecompilation<'s> {
    compilation: compilation::QueryCompilation<'s>,
    total_milliseconds: f64,
    read: &'s culprit_core::telemetry::CompilationSnapshot,
    source: Option<&'s culprit_core::SourceOutcome>,
    query: Option<&'s QueryStat>,
}

fn recorded_recompilations(snapshot: &Snapshot) -> Vec<RecordedRecompilation<'_>> {
    let Some(read) = snapshot
        .operations
        .query_compilation
        .as_ref()
        .filter(|read| read.is_valid() && read.fits_database(&snapshot.database_identity()))
    else {
        return Vec::new();
    };
    let rows = read.observations.iter().collect::<Vec<_>>();
    let source = snapshot.source_outcome(&read.source);
    let query_namespace_matches = read.matches_database_namespace(&snapshot.database_identity());
    let Ok(cases) = compilation::cases(&rows, source) else { return Vec::new() };
    let mut cases = cases
        .into_iter()
        .filter_map(|compilation| {
            let total_milliseconds = compilation.recompile?.total_milliseconds;
            let mut matches = snapshot.workload.iter().flat_map(|workload| &workload.queries).filter(|query| {
                query_namespace_matches && query.native_identity.as_ref() == Some(&compilation.native.identity)
            });
            let first = matches.next();
            let query = if matches.next().is_none() { first } else { None };
            if query.is_some_and(|query| crate::signals::comparable_planning_time(query).is_some()) {
                return None;
            }
            Some(RecordedRecompilation { compilation, total_milliseconds, read, source, query })
        })
        .collect::<Vec<_>>();
    cases.sort_by(|left, right| {
        right
            .total_milliseconds
            .total_cmp(&left.total_milliseconds)
            .then_with(|| left.compilation.observation.key.cmp(&right.compilation.observation.key))
    });
    cases.truncate(MAX_POLICY_FINDINGS);
    cases
}

fn compilation_policy_finding(snapshot: &Snapshot, case: RecordedRecompilation<'_>) -> Finding {
    let subject = match case.query {
        Some(query) => subject(SubjectKind::Query, &query.id),
        None => subject(SubjectKind::Database, &snapshot.server.database),
    };
    let mut finding = finding("compilation-policy", &case.compilation.observation.key, Category::Queries, subject);
    let rows = case.read.observations.iter().collect::<Vec<_>>();
    let limitations = compilation::limitations(
        &rows,
        case.source,
        true,
        rows.iter().any(|row| {
            matches!(&row.fact, culprit_core::telemetry::OperationalFact::MssqlQueryCompilation(native) if !native.statement.is_available())
        }),
    );
    let entry = compilation::render(case.compilation);
    finding.title = entry.title;
    finding.summary = entry.explanation;
    finding.metrics = entry.metrics;
    finding.fix = Some(Fix { summary: entry.investigation, sql: None, cost: None });
    finding.verify = vec![entry.verify];
    finding.caveats = limitations;
    if let Some(query) = case.query {
        finding.queries.push(culprit_core::QueryRef { id: query.id.clone(), plan_node: None, note: None });
    }
    finding.actionability = Actionability::Information;
    finding.severity = Severity::Low;
    finding.confidence = Confidence::High;
    finding.impact.score = base_score(Severity::Low);
    finding.impact.set_keyed_label(key!("check.compilation_policy.impact"), t!("check.compilation_policy.impact"));
    finding.facts.workload_dependency = Some(WorkloadDependency::Independent);
    finding.verification_basis = VerificationBasis::CounterChange;
    finding
}

#[cfg(test)]
mod tests;
