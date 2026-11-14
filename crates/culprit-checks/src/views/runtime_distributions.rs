//! Recorded quantiles describe their own populations, never a parameter cause, complete workload or safe delta.
use culprit_core::{
    Error, MeasurementScope, Metric, Result, RuntimeDistributionExplanation, RuntimeDistributionReport,
    RuntimeQuantile, SourceOutcome, SourceState, format, key, t,
    telemetry::{OperationalFact, OperationalObservation},
};
use std::collections::{BTreeMap, BTreeSet};

const MAX_OBSERVATIONS: usize = 256;

pub fn explain(
    observations: &[&OperationalObservation],
    sources: &[SourceOutcome],
) -> Result<RuntimeDistributionReport> {
    if observations.len() > MAX_OBSERVATIONS || sources.is_empty() || sources.len() > 128 {
        return Err(invalid());
    }
    let mut declared = BTreeMap::new();
    for source in sources {
        if declared.insert(source.source.as_str(), source).is_some() {
            return Err(invalid());
        }
        let count = observations.iter().filter(|row| row.source == source.source).count();
        if match source.state {
            SourceState::Collected => count == 0,
            SourceState::Empty | SourceState::Unavailable | SourceState::NotApplicable => count > 0,
            SourceState::Partial => false,
        } {
            return Err(invalid());
        }
    }
    let mut keys = BTreeSet::new();
    let mut entries = Vec::with_capacity(observations.len());
    for row in observations {
        if !row.is_valid() || !declared.contains_key(row.source.as_str()) || !keys.insert(&row.key) {
            return Err(invalid());
        }
        let OperationalFact::RuntimeDistribution(point) = &row.fact else { return Err(invalid()) };
        if point.scope != MeasurementScope::Database {
            return Err(invalid());
        }
        let distribution = &point.distribution;
        let mut metrics = Vec::new();
        for (quantile, sample, upper) in [
            (
                &distribution.p50,
                key!("view.runtime_distributions.metric.p50_sample"),
                key!("view.runtime_distributions.metric.p50_upper"),
            ),
            (
                &distribution.p95,
                key!("view.runtime_distributions.metric.p95_sample"),
                key!("view.runtime_distributions.metric.p95_upper"),
            ),
            (
                &distribution.p99,
                key!("view.runtime_distributions.metric.p99_sample"),
                key!("view.runtime_distributions.metric.p99_upper"),
            ),
        ] {
            if let Some(quantile) = quantile {
                let value = format::duration(quantile.upper_ms().ok_or_else(invalid)?);
                let label = if matches!(quantile, RuntimeQuantile::SampleValue { .. }) { sample } else { upper };
                metrics.push(Metric::said_text(label, value));
            }
        }
        if let Some(samples) = distribution.population.sample_count() {
            metrics.push(Metric::said_text(key!("view.runtime_distributions.metric.samples"), samples.to_string()));
        }
        entries.push(RuntimeDistributionExplanation {
            observation_key: row.key.clone(),
            identity: point.identity.clone(),
            observed_at: point.observed_at.clone(),
            reset: point.reset.clone(),
            title: t!("view.runtime_distributions.entry.title", statement = &point.identity.id),
            explanation: if distribution.is_usable() {
                t!("view.runtime_distributions.entry.observed", source = &distribution.source)
            } else {
                t!("view.runtime_distributions.entry.unavailable", source = &distribution.source)
            },
            metrics,
            investigation: t!("view.runtime_distributions.investigation"),
            verify: t!("view.runtime_distributions.verify"),
        });
    }
    entries.sort_by(|left, right| left.observation_key.cmp(&right.observation_key));
    let mut sources = sources.to_vec();
    sources.sort_by(|left, right| left.source.cmp(&right.source));
    let mut limitations = vec![
        t!("view.runtime_distributions.limit.population"),
        t!("view.runtime_distributions.limit.reset"),
        t!("view.runtime_distributions.limit.non_atomic"),
        t!("view.runtime_distributions.limit.identity"),
        t!("view.runtime_distributions.limit.monitoring"),
    ];
    if sources.iter().any(|source| source.state != SourceState::Collected && source.state != SourceState::Empty) {
        limitations.push(t!("view.runtime_distributions.limit.coverage"));
    }
    Ok(RuntimeDistributionReport {
        sources,
        title: t!("view.runtime_distributions.title"),
        summary: if entries.is_empty() {
            t!("view.runtime_distributions.summary.empty")
        } else {
            t!(
                "view.runtime_distributions.summary.observed",
                n = entries.len(),
                count = format::count(entries.len() as f64)
            )
        },
        limitations,
        entries,
    })
}

fn invalid() -> Error {
    Error::invalid(t!("collection.invalid_source"))
}

#[cfg(test)]
mod tests;
