//! Cleanup mechanisms retain their native populations and references without assigning SQL ownership.
use super::source_coverage;
use culprit_core::{
    CleanupBoundary, CleanupExplanation, CleanupReference, CleanupReport, Error, Result, SourceOutcome, SourceState,
    format, t, telemetry::*,
};
use std::{
    borrow::Cow,
    collections::{BTreeMap, BTreeSet},
};

mod persistent_versions;
mod retention;
mod transactions;

/// Sources passed to explain are the caller's exact selected cleanup inventory, including zero-row failures.
pub struct CleanupEvidence<'a> {
    pub retention: Option<&'a RetentionSnapshot>,
    pub native: &'a [&'a OperationalObservation],
}

pub fn explain(evidence: CleanupEvidence<'_>, sources: &[SourceOutcome]) -> Result<CleanupReport> {
    let sources = source_outcomes(&evidence, sources);
    let validated = validate(evidence, &sources)?;
    let mut entries = validated.holders.iter().map(|row| retention::explain(row)).collect::<Result<Vec<_>>>()?;
    for row in &validated.points {
        let entry = match &row.fact {
            OperationalFact::MssqlPersistentVersionStore(store) => persistent_versions::explain(row, store),
            OperationalFact::MssqlCleanupHorizon(horizon) => transactions::explain(row, horizon),
            _ => return Err(invalid()),
        };
        entries.push(entry);
    }
    entries.sort_by(|left, right| {
        (&left.reference.source, &left.reference.observation_key)
            .cmp(&(&right.reference.source, &right.reference.observation_key))
    });
    let mut limitations = vec![t!("view.cleanup.limit.point"), t!("view.cleanup.limit.ownership")];
    if !validated.points.is_empty() {
        limitations.push(t!("view.cleanup.limit.native_clocks"));
        limitations.push(t!("view.cleanup.limit.independent_reads"));
    }
    let boundaries = retention::boundaries(validated.population.as_deref(), &sources, validated.metadata_complete)?;
    if validated.has_retention {
        limitations.push(t!("view.cleanup.limit.horizon_units"));
        if validated.population.is_none() || !validated.metadata_complete {
            limitations.push(t!("view.cleanup.limit.coverage_unknown"));
        }
        if validated.holders.iter().any(|row| row.horizons.iter().any(|horizon| horizon.transactions_behind.is_none()))
        {
            limitations.push(t!("view.cleanup.limit.order_unknown"));
        }
    }
    if validated.unknown_source {
        limitations.push(t!("view.cleanup.limit.source_unknown"));
    }
    if sources.iter().any(|source| source.state == SourceState::Partial) {
        limitations.push(t!("view.cleanup.limit.source_partial"));
    }
    if sources.iter().any(|source| matches!(source.state, SourceState::Unavailable | SourceState::NotApplicable)) {
        limitations.push(t!("view.cleanup.limit.source_unavailable"));
    }
    Ok(CleanupReport {
        sources,
        title: t!("view.cleanup.title"),
        summary: if entries.is_empty() {
            t!("view.cleanup.summary.empty")
        } else {
            t!("view.cleanup.summary.entries", n = entries.len(), count = format::digits(entries.len().to_string()))
        },
        limitations,
        boundaries,
        entries,
    })
}

pub(crate) fn native_source_is_consistent(source: Option<&SourceOutcome>, rows: &[&OperationalObservation]) -> bool {
    if source.is_some_and(|source| source.state == SourceState::Empty && rows.iter().all(|row| row.source == source.source))
        && rows.iter().all(|row| matches!(&row.fact, OperationalFact::MssqlCleanupHorizon(horizon) if horizon.oldest_confirmed.is_none() && horizon.oldest_unattributed_versioning.is_none()))
    {
        return source_coverage::consistent_source(source, std::iter::empty());
    }
    source_coverage::consistent_source(source, rows.iter().map(|row| row.source.as_str()))
}

pub(crate) fn retention_source_is_consistent(
    source: Option<&SourceOutcome>,
    holders: &[&RetentionObservation],
) -> bool {
    source_coverage::consistent_source(source, holders.iter().map(|row| row.source.as_str()))
}

pub(crate) fn is_cleanup_source(source: &str) -> bool {
    use culprit_core::source_id;
    [
        source_id::OPEN_TRANSACTIONS,
        source_id::PERSISTENT_VERSION_STORE,
        source_id::CLEANUP_SLOTS,
        source_id::CLEANUP_PREPARED,
        RetentionCoverage::SOURCE,
    ]
    .contains(&source)
}

fn source_outcomes(evidence: &CleanupEvidence<'_>, sources: &[SourceOutcome]) -> Vec<SourceOutcome> {
    let mut selected = sources.to_vec();
    if let Some(population) = evidence.retention {
        for source in &population.sources {
            if !selected.iter().any(|outcome| outcome.source == source.source) {
                selected.push(SourceOutcome {
                    source: source.source.clone(),
                    state: source.state,
                    reason: None,
                    reason_message: None,
                });
            }
        }
    }
    selected
}

struct Validated<'a> {
    holders: Vec<&'a RetentionObservation>,
    points: Vec<&'a OperationalObservation>,
    population: Option<Cow<'a, RetentionSnapshot>>,
    metadata_complete: bool,
    has_retention: bool,
    unknown_source: bool,
}

fn validate<'a>(evidence: CleanupEvidence<'a>, sources: &[SourceOutcome]) -> Result<Validated<'a>> {
    if evidence.native.len() > MAX_CLEANUP_OBSERVATIONS || sources.len() > 128 {
        return Err(invalid());
    }
    let mut outcomes = BTreeMap::new();
    for source in sources {
        if source.source.is_empty()
            || source.source.len() > 512
            || source.source.chars().any(char::is_control)
            || outcomes.insert(source.source.as_str(), source).is_some()
        {
            return Err(invalid());
        }
    }
    let mut keys = BTreeSet::new();
    let mut counts = BTreeMap::<String, usize>::new();
    let mut holders = Vec::new();
    if let Some(population) = evidence.retention {
        if !population.is_valid() {
            return Err(invalid());
        }
        for holder in &population.holders {
            if !keys.insert((holder.source.clone(), holder.key()?)) {
                return Err(invalid());
            }
            *counts.entry(holder.source.clone()).or_default() += 1;
            holders.push(holder);
        }
        if outcomes.contains_key(RetentionCoverage::SOURCE) {
            counts.insert(RetentionCoverage::SOURCE.into(), 1);
        }
    }
    let mut points = Vec::new();
    let mut metadata = None;
    for row in evidence.native {
        if !row.is_valid() || !keys.insert((row.source.clone(), row.key.clone())) {
            return Err(invalid());
        }
        *counts.entry(row.source.clone()).or_default() += 1;
        match &row.fact {
            OperationalFact::Retention(holder) => {
                if evidence.retention.is_some() || holder.source != row.source {
                    return Err(invalid());
                }
                holders.push(holder.as_ref());
            }
            OperationalFact::RetentionCoverage(coverage) => {
                if evidence.retention.is_some()
                    || row.source != RetentionCoverage::SOURCE
                    || metadata.replace(coverage).is_some()
                {
                    return Err(invalid());
                }
            }
            OperationalFact::MssqlCleanupHorizon(_) => points.push(*row),
            OperationalFact::MssqlPersistentVersionStore(_) => points.push(*row),
            _ => return Err(invalid()),
        }
    }
    if holders.len() > MAX_RETENTION_HOLDERS {
        return Err(invalid());
    }
    for source in sources {
        let native = evidence.native.iter().copied().filter(|row| row.source == source.source).collect::<Vec<_>>();
        if !native.is_empty() {
            if !native_source_is_consistent(Some(source), &native) {
                return Err(invalid());
            }
        } else if evidence
            .retention
            .is_some_and(|population| population.sources.iter().any(|native| native.source == source.source))
        {
            let selected = holders.iter().copied().filter(|holder| holder.source == source.source).collect::<Vec<_>>();
            if !retention_source_is_consistent(Some(source), &selected) {
                return Err(invalid());
            }
        }
    }
    let metadata_complete = if evidence.retention.is_some() {
        outcomes.get(RetentionCoverage::SOURCE).is_none_or(|source| source.state == SourceState::Collected)
    } else {
        outcomes.get(RetentionCoverage::SOURCE).is_some_and(|source| source.state == SourceState::Collected)
    };
    let population = if let Some(population) = evidence.retention {
        Some(Cow::Borrowed(population))
    } else if let Some(coverage) = metadata {
        if holders.iter().any(|holder| !coverage.native_sources.contains(&holder.source)) {
            return Err(invalid());
        }
        let states = coverage
            .native_sources
            .iter()
            .map(|source| {
                outcomes
                    .get(source.as_str())
                    .map(|outcome| RetentionSourceCoverage { source: source.clone(), state: outcome.state })
            })
            .collect::<Option<Vec<_>>>();
        if let Some(states) = states {
            let population = RetentionSnapshot {
                holders: holders.iter().map(|row| (*row).clone()).collect(),
                sources: states,
                coverage: coverage.clone(),
            };
            if !population.is_valid() {
                return Err(invalid());
            }
            Some(Cow::Owned(population))
        } else {
            None
        }
    } else {
        None
    };
    Ok(Validated {
        has_retention: evidence.retention.is_some() || metadata.is_some() || !holders.is_empty(),
        unknown_source: sources.is_empty()
            || counts.keys().any(|source| !outcomes.contains_key(source.as_str()))
            || metadata.is_some_and(|metadata| {
                metadata.native_sources.iter().any(|source| !outcomes.contains_key(source.as_str()))
            }),
        holders,
        points,
        population,
        metadata_complete,
    })
}

fn native_reference(row: &OperationalObservation) -> CleanupReference {
    CleanupReference { source: row.source.clone(), observation_key: row.key.clone(), identity: None }
}

fn invalid() -> Error {
    Error::invalid(t!("view.cleanup.invalid"))
}

#[cfg(test)]
mod tests;
