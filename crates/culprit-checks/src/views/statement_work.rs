//! Native statement counter points describe measured work without inventing a query runtime or a safe delta.
use super::temporary_work;
use culprit_core::{
    Error, MeasurementScope, Result, SourceOutcome, SourceState, StatementWorkEntry, StatementWorkReport, format, t,
    telemetry::{OperationalFact, OperationalObservation, statement_temporary_work},
};
use std::collections::BTreeSet;

pub fn explain(
    observations: &[&OperationalObservation],
    source: Option<&SourceOutcome>,
) -> Result<StatementWorkReport> {
    let expected =
        source.map(|source| source.source.as_str()).or_else(|| observations.first().map(|row| row.source.as_str()));
    if source.is_some_and(|source| match source.state {
        SourceState::Empty | SourceState::Unavailable | SourceState::NotApplicable => !observations.is_empty(),
        SourceState::Collected => observations.is_empty(),
        SourceState::Partial => false,
    }) {
        return Err(invalid());
    }
    let mut keys = BTreeSet::new();
    let mut entries = Vec::with_capacity(observations.len());
    for observation in observations {
        if !observation.is_valid() || expected != Some(observation.source.as_str()) || !keys.insert(&observation.key) {
            return Err(invalid());
        }
        let OperationalFact::StatementCounters(sample) = &observation.fact else { return Err(invalid()) };
        if sample.scope != MeasurementScope::Database {
            return Err(invalid());
        }
        let work = statement_temporary_work(sample, &observation.source).map_err(|_| invalid())?;
        let work = work
            .iter()
            .map(|work| temporary_work::evidence(work).map(temporary_work::explain).ok_or_else(invalid))
            .collect::<Result<Vec<_>>>()?;
        entries.push(StatementWorkEntry {
            observation_key: observation.key.clone(),
            identity: sample.identity.clone(),
            observed_at: sample.observed_at.clone(),
            reset: sample.reset.clone(),
            work,
        });
    }
    entries.sort_by(|left, right| left.observation_key.cmp(&right.observation_key));
    let mut limitations = vec![
        t!("view.statement_work.limit.point"),
        t!("view.statement_work.limit.reset"),
        t!("view.statement_work.limit.identity"),
    ];
    if entries.iter().any(|entry| entry.work.is_empty()) {
        limitations.push(t!("view.statement_work.limit.unmeasured"));
    }
    match source.map(|source| source.state) {
        None => limitations.push(t!("view.statement_work.limit.source_unknown")),
        Some(SourceState::Partial) => limitations.push(t!("view.statement_work.limit.source_partial")),
        Some(SourceState::Unavailable | SourceState::NotApplicable) => {
            limitations.push(t!("view.statement_work.limit.source_unavailable"))
        }
        Some(SourceState::Collected | SourceState::Empty) => {}
    }
    Ok(StatementWorkReport {
        source_state: source.map(|source| source.state),
        source_reason: source.and_then(|source| source.reason.clone()),
        title: t!("view.statement_work.title"),
        summary: if entries.is_empty() {
            t!("view.statement_work.summary.empty")
        } else {
            t!("view.statement_work.summary.observed", n = entries.len(), count = format::count(entries.len() as f64))
        },
        limitations,
        entries,
    })
}

fn invalid() -> Error {
    Error::invalid(t!("view.statement_work.invalid"))
}

#[cfg(test)]
mod tests;
