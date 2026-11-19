//! Retained per-query compilation measurements are historical facts, not a current allocation or recompile cause.
use super::source_coverage;
use culprit_core::{
    CompilationExplanation, CompilationReport, Error, Metric, Result, SourceOutcome, SourceState, format, key, t,
    telemetry::{MAX_QUERY_COMPILATION_OBSERVATIONS, MssqlQueryCompilation, OperationalFact, OperationalObservation},
};
use std::collections::BTreeSet;

pub(crate) mod policy;
pub(crate) use policy::{RepeatedRecompile, repeated_recompile};
mod comparison;
pub use comparison::compare;
mod definitions;
mod snapshot;
mod timings;
pub use super::query_store_definitions::{QueryStoreDefinitionEvidence, QueryStoreDefinitionRecord};
pub(crate) use snapshot::{CapturedCompilations, workload_query};

/// Native compiler history and an optional independently authenticated definition selection. Definition intervals
/// remain runtime history; only the original compiler context supplies native compiler read bounds.
#[derive(Clone, Copy)]
pub struct CompilationEvidence<'a> {
    pub context: Option<&'a culprit_core::telemetry::NativeReadContext>,
    pub observations: &'a [&'a OperationalObservation],
    pub source: Option<&'a SourceOutcome>,
    pub definitions: Option<QueryStoreDefinitionEvidence<'a>>,
}

pub fn explain(input: CompilationEvidence<'_>) -> Result<CompilationReport> {
    validate_context(input.context)?;
    let definitions =
        input.definitions.map(|definitions| definitions::Definitions::new(input.context, definitions)).transpose()?;
    let mut cases = qualified_cases(input.observations, input.source, definitions.as_ref())?;
    cases.sort_by(|left, right| left.observation.key.cmp(&right.observation.key));
    let mut limitations = limitations(
        input.observations,
        input.source,
        cases.iter().any(|case| case.recompile.is_some()),
        cases.iter().any(|case| !case.statement.payload.is_available()),
    );
    if let Some(definitions) = &definitions {
        definitions.append_limitations(&mut limitations);
    }
    Ok(CompilationReport {
        source_state: input.source.map(|source| source.state),
        source_reason: input.source.and_then(|source| source.reason.clone()),
        title: t!("view.compilation.title"),
        summary: if input.observations.is_empty() {
            t!("view.compilation.summary.empty")
        } else {
            t!(
                "view.compilation.summary.entries",
                n = input.observations.len(),
                count = format::digits(input.observations.len().to_string())
            )
        },
        limitations,
        entries: cases.into_iter().map(render).collect(),
        definition_read: definitions.as_ref().map(|definitions| Box::new(definitions.report())),
        comparison: None,
    })
}

pub(crate) fn limitations(
    observations: &[&OperationalObservation],
    source: Option<&SourceOutcome>,
    explicit_policy: bool,
    statement_unavailable: bool,
) -> Vec<String> {
    let mut limitations = vec![
        t!("view.compilation.limit.lifetime"),
        t!("view.compilation.limit.population"),
        if explicit_policy { t!("view.compilation.limit.policy") } else { t!("view.compilation.limit.causes") },
        t!("check.planning_time.caveat.compile_memory"),
        t!("view.compilation.limit.admission"),
        t!("view.compilation.limit.stage_timings"),
    ];
    if observations.iter().any(|row| {
        matches!(&row.fact, OperationalFact::MssqlQueryCompilation(native) if native.seconds_since_first_compile < 0)
    }) {
        limitations.push(t!("view.compilation.limit.clock"));
    }
    if statement_unavailable {
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
    statement: definitions::Statement<'a>,
}

pub(crate) fn cases<'a>(
    observations: &[&'a OperationalObservation],
    source: Option<&SourceOutcome>,
) -> Result<Vec<QueryCompilation<'a>>> {
    qualified_cases(observations, source, None)
}

fn qualified_cases<'a>(
    observations: &[&'a OperationalObservation],
    source: Option<&SourceOutcome>,
    definitions: Option<&definitions::Definitions<'a>>,
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
            let statement = definitions
                .map(|definitions| definitions.resolve(native))
                .transpose()?
                .unwrap_or_else(|| definitions::Statement::captured(native));
            let recompile = if definitions.is_some() {
                policy::with_statement(native, statement.payload, statement.origin)
            } else {
                repeated_recompile(native)
            };
            Ok(QueryCompilation {
                observation,
                native,
                count: native.compilations.parse().map_err(|_| invalid())?,
                first_compile_at: native.first_compile_at().map_err(|_| invalid())?,
                recompile,
                statement,
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
    metrics.extend(timings::metrics(row));
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
        statement: case.statement.report,
    }
}

fn invalid() -> Error {
    Error::invalid(t!("view.compilation.invalid"))
}

fn validate_context(context: Option<&culprit_core::telemetry::NativeReadContext>) -> Result<()> {
    if context.is_some_and(|context| {
        !context.is_valid()
            || context.target.engine() != culprit_core::Engine::Mssql
            || context.target.database().is_none()
            || context.partition != culprit_core::telemetry::SourcePartition::Target
    }) {
        return Err(invalid());
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests;
