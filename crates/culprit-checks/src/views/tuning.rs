//! Native tuning points explain their own configuration and counters, independently of query workloads.
use culprit_core::{
    AutomaticTuningOption, Error, Result, SourceOutcome, SourceState, TuningExplanation, TuningReport,
    i18n::{Message, MessageValue},
    message,
    telemetry::{
        CollectionBudget, NativeForcedPlan, NativeQueryHint, NativeTuningRecommendation, TuningFact, TuningObservation,
    },
};
use std::collections::BTreeSet;

pub fn explain(observations: &[&TuningObservation], sources: &[SourceOutcome]) -> Result<TuningReport> {
    let mut cases = validate(observations, sources)?;
    cases
        .sort_by(|left, right| (&left.row.source, left.row.fact.key()).cmp(&(&right.row.source, right.row.fact.key())));
    let mut limitations = Vec::new();
    if observations.iter().any(|row| !row.fact.attribution_is_complete()) {
        limitations.push(message!("view.tuning.limit.scope"));
    }
    limitations.extend([message!("view.tuning.limit.execution"), message!("view.tuning.limit.management")]);
    if observations.iter().any(|row| match &row.fact {
        TuningFact::Recommendation(value) => !value.script.is_available(),
        TuningFact::QueryHint(value) => !value.hint.is_available(),
        _ => false,
    }) {
        limitations.push(message!("view.tuning.limit.definition"));
    }
    if sources.is_empty() || observations.iter().any(|row| !sources.iter().any(|source| source.source == row.source)) {
        limitations.push(message!("view.tuning.limit.source_unknown"));
    }
    if sources.iter().any(|source| source.state == SourceState::Partial) {
        limitations.push(message!("view.tuning.limit.source_partial"));
    }
    if sources.iter().any(|source| source.state == SourceState::Unavailable) {
        limitations.push(message!("view.tuning.limit.source_unavailable"));
    }
    if sources.iter().any(|source| source.state == SourceState::NotApplicable) {
        limitations.push(message!("view.tuning.limit.source_not_applicable"));
    }
    Ok(TuningReport {
        sources: sources.to_vec(),
        title: message!("view.tuning.title"),
        summary: if observations.is_empty() {
            message!("view.tuning.summary.empty")
        } else {
            message!("view.tuning.summary.rows", count = MessageValue::Digits(observations.len().to_string()))
        },
        limitations,
        entries: cases.into_iter().map(render).collect(),
    })
}

struct Case<'a> {
    row: &'a TuningObservation,
    detail: Detail<'a>,
}
enum Detail<'a> {
    Automatic(&'a AutomaticTuningOption),
    Recommendation(&'a NativeTuningRecommendation),
    Forced { native: &'a NativeForcedPlan, failures: u64 },
    Hint { native: &'a NativeQueryHint, failures: u64 },
}

fn validate<'a>(rows: &[&'a TuningObservation], sources: &[SourceOutcome]) -> Result<Vec<Case<'a>>> {
    if rows.len() > CollectionBudget::upper_limits().max_snapshot_rows as usize {
        return Err(invalid());
    }
    let mut source_names = BTreeSet::new();
    for source in sources {
        if source.source.trim().is_empty()
            || source.source.len() > 512
            || source.source.chars().any(char::is_control)
            || !source_names.insert(&source.source)
            || !super::source_coverage::consistent_source(
                Some(source),
                rows.iter().filter(|row| row.source == source.source).map(|row| row.source.as_str()),
            )
        {
            return Err(invalid());
        }
    }
    let mut keys = BTreeSet::new();
    rows.iter()
        .map(|&row| {
            if !row.is_valid() || !keys.insert((&row.source, row.fact.key())) {
                return Err(invalid());
            }
            let detail = match &row.fact {
                TuningFact::AutomaticOption(value) => Detail::Automatic(value),
                TuningFact::Recommendation(value) => Detail::Recommendation(value),
                TuningFact::ForcedPlan(native) => {
                    Detail::Forced { native, failures: native.failures.parse().map_err(|_| invalid())? }
                }
                TuningFact::QueryHint(native) => {
                    Detail::Hint { native, failures: native.failures.parse().map_err(|_| invalid())? }
                }
            };
            Ok(Case { row, detail })
        })
        .collect()
}

fn render(case: Case<'_>) -> TuningExplanation {
    let (scope, title, explanation, investigation) = match case.detail {
        Detail::Automatic(value) => (
            None,
            message!("view.tuning.option.title", name = &value.name),
            message!(
                "view.tuning.option.explanation",
                desired = &value.desired,
                actual = &value.actual,
                has_reason = if value.reason.is_some() { "yes" } else { "no" },
                reason = value.reason.as_deref().unwrap_or_default()
            ),
            message!("view.tuning.option.investigation"),
        ),
        Detail::Recommendation(value) => {
            let mut explanation = vec![message!(
                "view.tuning.recommendation.explanation",
                kind = &value.kind,
                has_state = if value.state.is_some() { "yes" } else { "no" },
                state = value.state.as_deref().unwrap_or_default()
            )];
            if let Some(reduction) = value.estimated_cpu_reduction().filter(|value| *value > 0.0) {
                explanation.push(message!("view.tuning.recommendation.cpu", gain = MessageValue::Percent(reduction)));
            }
            if value.regressed_has_more_aborted_executions() {
                explanation.push(message!("view.tuning.recommendation.aborted"));
            }
            (
                Some(value.plan_scope.clone()),
                message!("view.tuning.recommendation.title", name = &value.name),
                Message::sequence(explanation, " "),
                message!("view.tuning.recommendation.investigation"),
            )
        }
        Detail::Forced { native, failures } => (
            Some(native.plan_scope.clone()),
            if failures > 0 { message!("view.tuning.forced.failures") } else { message!("view.tuning.forced.title") },
            message!(
                "view.tuning.forced.explanation",
                query = &native.query_id,
                plan = &native.plan_id,
                count = MessageValue::Digits(native.failures.clone()),
                has_reason = if native.last_failure_reason.is_some() { "yes" } else { "no" },
                reason = native.last_failure_reason.as_deref().unwrap_or_default()
            ),
            message!("view.tuning.configuration.investigation"),
        ),
        Detail::Hint { native, failures } => (
            Some(native.scope.clone()),
            if failures > 0 { message!("view.tuning.hint.failures") } else { message!("view.tuning.hint.title") },
            message!(
                "view.tuning.hint.explanation",
                query = &native.query_id,
                hint = &native.hint_id,
                count = MessageValue::Digits(native.failures.clone()),
                has_reason = if native.last_failure_reason.is_some() { "yes" } else { "no" },
                reason = native.last_failure_reason.as_deref().unwrap_or_default()
            ),
            message!("view.tuning.configuration.investigation"),
        ),
    };
    TuningExplanation {
        source: case.row.source.clone(),
        observation_key: case.row.fact.key(),
        scope,
        title,
        explanation,
        investigation,
        verify: message!("view.tuning.verify"),
    }
}
fn invalid() -> Error {
    culprit_core::telemetry::CollectionStop::InvalidPage.error()
}

#[cfg(test)]
mod tests;
