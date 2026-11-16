//! Native statement points describe exact recorded work and outcomes, without an inferred interval or cause.
use super::temporary_work;
use culprit_core::{
    Error, Metric, Result, SourceState, StatementWorkEntry, StatementWorkReport, format, key, t,
    telemetry::{OperationalFact, StatementCounterSnapshot},
};

pub fn explain(snapshot: &StatementCounterSnapshot, read_complete: bool) -> Result<StatementWorkReport> {
    if !snapshot.is_valid_read(read_complete) {
        return Err(invalid());
    }
    let mut entries = Vec::with_capacity(snapshot.observations.len());
    for observation in &snapshot.observations {
        let OperationalFact::StatementCounters(sample) = &observation.fact else { return Err(invalid()) };
        let work =
            culprit_core::telemetry::statement_temporary_work(sample, &observation.source).map_err(|_| invalid())?;
        let work = work
            .iter()
            .map(|work| temporary_work::evidence(work).map(temporary_work::explain).ok_or_else(invalid))
            .collect::<Result<Vec<_>>>()?;
        let outcomes = sample.outcomes().map_err(|_| invalid())?;
        let mut outcome_metrics = Vec::new();
        for (label, value) in [
            (key!("view.statement_work.outcome.errors"), &outcomes.errors),
            (key!("view.statement_work.outcome.warnings"), &outcomes.warnings),
        ] {
            if let Some(value) = value {
                outcome_metrics.push(Metric::said_text(label, format::digits(value.clone())));
            }
        }
        entries.push(StatementWorkEntry {
            source: observation.source.clone(),
            observation_key: observation.key.clone(),
            identity: sample.identity.clone(),
            scope: sample.scope,
            observed_at: sample.observed_at.clone(),
            reset: sample.reset.clone(),
            work,
            outcomes,
            outcome_metrics,
        });
    }
    entries.sort_by(|left, right| left.observation_key.cmp(&right.observation_key));
    let mut limitations = vec![
        t!("view.statement_work.limit.point"),
        t!("view.statement_work.limit.reset"),
        t!("view.statement_work.limit.identity"),
    ];
    if snapshot.context.is_none() {
        limitations.push(t!("view.statement_work.limit.source_unknown"));
    }
    if entries.iter().any(|entry| entry.scope == culprit_core::MeasurementScope::Node) {
        limitations.push(t!("view.statement_work.limit.node"));
    }
    if entries.iter().any(|entry| entry.work.is_empty()) {
        limitations.push(t!("view.statement_work.limit.unmeasured"));
    }
    if entries.iter().any(|entry| entry.outcomes.is_recorded()) {
        limitations.push(t!("view.statement_work.limit.outcomes"));
    }
    let missing = !read_complete || snapshot.observations.len() != snapshot.coverage.records as usize;
    let state = if missing
        && matches!(snapshot.coverage.state, SourceState::Collected | SourceState::Empty | SourceState::Partial)
    {
        SourceState::Partial
    } else {
        snapshot.coverage.state
    };
    if missing {
        limitations.push(t!("view.statement_work.limit.read"));
    }
    match state {
        SourceState::Partial => limitations.push(t!("view.statement_work.limit.source_partial")),
        SourceState::Unavailable | SourceState::NotApplicable => {
            limitations.push(t!("view.statement_work.limit.source_unavailable"))
        }
        SourceState::Collected | SourceState::Empty => {}
    }
    Ok(StatementWorkReport {
        source: snapshot.source.clone(),
        context: snapshot.context.clone(),
        source_state: Some(state),
        source_reason: snapshot.coverage.error_code.map(|_| t!("view.statement_work.source_error")),
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
