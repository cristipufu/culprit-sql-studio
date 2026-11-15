//! Retained per-query compilation measurements are historical facts, not a current allocation or recompile cause.
use super::source_coverage;
use culprit_core::{
    CompilationExplanation, CompilationReport, Error, Metric, Result, SourceOutcome, SourceState, format, key, t,
    telemetry::{MAX_QUERY_COMPILATION_OBSERVATIONS, MssqlQueryCompilation, OperationalFact, OperationalObservation},
};
use std::collections::BTreeSet;

pub(crate) mod policy;
pub(crate) use policy::{RepeatedRecompile, repeated_recompile};

pub fn explain(observations: &[&OperationalObservation], source: Option<&SourceOutcome>) -> Result<CompilationReport> {
    let mut cases = cases(observations, source)?;
    cases.sort_by(|left, right| left.observation.key.cmp(&right.observation.key));
    let limitations = limitations(observations, source, cases.iter().any(|case| case.recompile.is_some()));
    Ok(CompilationReport {
        source_state: source.map(|source| source.state),
        source_reason: source.and_then(|source| source.reason.clone()),
        title: t!("view.compilation.title"),
        summary: if observations.is_empty() {
            t!("view.compilation.summary.empty")
        } else {
            t!(
                "view.compilation.summary.entries",
                n = observations.len(),
                count = format::digits(observations.len().to_string())
            )
        },
        limitations,
        entries: cases.into_iter().map(render).collect(),
    })
}

pub(crate) fn limitations(
    observations: &[&OperationalObservation],
    source: Option<&SourceOutcome>,
    explicit_policy: bool,
) -> Vec<String> {
    let mut limitations = vec![
        t!("view.compilation.limit.lifetime"),
        t!("view.compilation.limit.population"),
        if explicit_policy { t!("view.compilation.limit.policy") } else { t!("view.compilation.limit.causes") },
        t!("check.planning_time.caveat.compile_memory"),
        t!("view.compilation.limit.admission"),
    ];
    if observations.iter().any(|row| {
        matches!(&row.fact, OperationalFact::MssqlQueryCompilation(native) if native.seconds_since_first_compile < 0)
    }) {
        limitations.push(t!("view.compilation.limit.clock"));
    }
    if observations.iter().any(
        |row| matches!(&row.fact, OperationalFact::MssqlQueryCompilation(native) if !native.statement.is_available()),
    ) {
        limitations.push(t!("view.compilation.limit.statement"));
    }
    match source.map(|source| source.state) {
        None => limitations.push(t!("view.compilation.limit.source_unknown")),
        Some(SourceState::Partial) => limitations.push(t!("view.compilation.limit.source_partial")),
        Some(SourceState::Unavailable | SourceState::NotApplicable) => {
            limitations.push(t!("view.compilation.limit.source_unavailable"))
        }
        Some(SourceState::Collected | SourceState::Empty) => {}
    }
    limitations
}

pub(crate) struct QueryCompilation<'a> {
    pub observation: &'a OperationalObservation,
    pub native: &'a MssqlQueryCompilation,
    count: u64,
    first_compile_at: String,
    pub recompile: Option<RepeatedRecompile<'a>>,
}

pub(crate) fn cases<'a>(
    observations: &[&'a OperationalObservation],
    source: Option<&SourceOutcome>,
) -> Result<Vec<QueryCompilation<'a>>> {
    if observations.len() > MAX_QUERY_COMPILATION_OBSERVATIONS
        || !source_coverage::consistent_source(source, observations.iter().map(|row| row.source.as_str()))
    {
        return Err(invalid());
    }
    let mut keys = BTreeSet::new();
    observations
        .iter()
        .map(|observation| {
            if !observation.is_valid() || !keys.insert(&observation.key) {
                return Err(invalid());
            }
            let OperationalFact::MssqlQueryCompilation(native) = &observation.fact else { return Err(invalid()) };
            Ok(QueryCompilation {
                observation,
                native,
                count: native.compilations.parse().map_err(|_| invalid())?,
                first_compile_at: native.first_compile_at().map_err(|_| invalid())?,
                recompile: repeated_recompile(native),
            })
        })
        .collect()
}

pub(crate) fn render(case: QueryCompilation<'_>) -> CompilationExplanation {
    let row = case.native;
    let count = format::digits(row.compilations.clone());
    let mut explanation = vec![match case.count {
        0 => t!("view.compilation.entry.zero"),
        1 => t!("view.compilation.entry.single"),
        _ => t!("view.compilation.entry.multiple", count = &count),
    }];
    let mut metrics = vec![
        Metric::said_text(key!("view.compilation.metric.count"), count),
        Metric::said_text(key!("view.compilation.metric.first"), case.first_compile_at),
        Metric::said_text(key!("view.compilation.metric.last"), row.last_compile_at.clone()),
        Metric::said_text(
            key!("view.compilation.metric.average_duration"),
            t!("common.duration.us", n = format::digits(row.average_duration_us.to_string())),
        ),
    ];
    if let Some(recompile) = case.recompile {
        explanation
            .push(t!("view.compilation.entry.recompile", total = format::duration(recompile.total_milliseconds)));
        metrics.push(Metric::said_text(
            key!("view.compilation.metric.lifetime_duration"),
            format::duration(recompile.total_milliseconds),
        ));
    }
    if let Some(memory) = row.memory {
        for (label, value) in [
            (key!("common.metric.compile_memory_average"), memory.average_kb),
            (key!("common.metric.compile_memory_last"), memory.last_kb),
            (key!("common.metric.compile_memory_maximum"), memory.maximum_kb),
        ] {
            if let Some(value) = value {
                metrics.push(Metric::said_text(label, t!("common.bytes.kib", n = format::digits(value.to_string()))));
            }
        }
    } else {
        explanation.push(t!("view.compilation.entry.memory_unknown"));
    }
    CompilationExplanation {
        observation_key: case.observation.key.clone(),
        identity: row.identity.clone(),
        title: if let Some(recompile) = case.recompile {
            const TITLE_CHARACTERS: usize = 160;
            let mut statement: String = recompile.statement.chars().take(TITLE_CHARACTERS).collect();
            if recompile.statement.chars().count() > TITLE_CHARACTERS {
                statement.push('…');
            }
            t!("view.compilation.entry.recompile_title", statement = statement)
        } else {
            t!("view.compilation.entry.title", query = &row.identity.query_id)
        },
        explanation: explanation.join(" "),
        metrics,
        investigation: if case.recompile.is_some() {
            t!("check.planning_time.fix.recompile_hint")
        } else if case.count > 1 {
            t!("view.compilation.investigate.multiple")
        } else {
            t!("view.compilation.investigate.retained")
        },
        verify: if case.recompile.is_some() {
            t!("view.compilation.verify.recompile")
        } else {
            t!("view.compilation.verify")
        },
    }
}

fn invalid() -> Error {
    Error::invalid(t!("view.compilation.invalid"))
}

#[cfg(test)]
pub(crate) mod tests;
