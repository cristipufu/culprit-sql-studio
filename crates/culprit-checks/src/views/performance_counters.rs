//! Native scalar inputs describe one shared population; they establish neither a recent rate nor a query cause.
use culprit_core::{
    Engine, Error, Metric, PerformanceCounterReport, Result, Snapshot, SourceOutcome, SourceState, format, t,
    telemetry::*,
};
use std::collections::BTreeSet;
mod entries;

pub const MAX_PERFORMANCE_COUNTER_ENTRIES: usize = 256;

pub enum PerformanceCounterEvidence<'a> {
    Captured {
        context: &'a NativeReadContext,
        coverage: &'a StateSourceCoverage,
        observations: &'a [&'a OperationalObservation],
        read_complete: bool,
    },
    Database {
        source: &'a CollectionDescription,
        coverage: &'a StateSourceCoverage,
        observations: &'a [&'a OperationalObservation],
        read_complete: bool,
    },
    Snapshot {
        engine: Engine,
        samples: &'a [&'a NativeCounterSet],
        source: Option<&'a SourceOutcome>,
    },
}

pub fn explain_snapshot(snapshot: &Snapshot) -> Result<Option<PerformanceCounterReport>> {
    let profile = snapshot.server.engine.spec().performance_counters();
    let samples =
        snapshot.pressure.as_ref().and_then(|pressure| pressure.resource_counters.as_deref()).unwrap_or_default();
    let samples =
        samples.iter().filter(|sample| profile.is_some_and(|profile| profile.candidate(sample))).collect::<Vec<_>>();
    let source = snapshot.sources.iter().find(|source| source.source == culprit_core::source_id::PERFORMANCE_COUNTERS);
    if samples.is_empty() && source.is_none() {
        return Ok(None);
    }
    explain(PerformanceCounterEvidence::Snapshot { engine: snapshot.server.engine, samples: &samples, source })
        .map(Some)
}

/// Both local originals and warehouse adapters supply the same original source and native rows.
pub fn explain_source(
    source: &CollectionDescription,
    coverage: &StateSourceCoverage,
    observations: &[&OperationalObservation],
    read_complete: bool,
) -> Result<PerformanceCounterReport> {
    let context = NativeReadContext::from_snapshot(source).map_err(|_| invalid())?;
    let evidence = match source.observation.snapshot().ok_or_else(invalid)?.dataset {
        SnapshotDataset::DatabasePerformanceCounters => {
            PerformanceCounterEvidence::Database { source, coverage, observations, read_complete }
        }
        SnapshotDataset::Operational => {
            PerformanceCounterEvidence::Captured { context: &context, coverage, observations, read_complete }
        }
        _ => return Err(invalid()),
    };
    explain(evidence)
}

pub fn explain(evidence: PerformanceCounterEvidence<'_>) -> Result<PerformanceCounterReport> {
    match evidence {
        PerformanceCounterEvidence::Snapshot { engine, samples, source } => {
            render(engine, samples, source, None, false)
        }
        PerformanceCounterEvidence::Database { source, coverage, observations, read_complete } => {
            let context = database_performance_context(source).map_err(|_| invalid())?;
            let snapshot = source.observation.snapshot().ok_or_else(invalid)?;
            if snapshot.sources.as_slice() != [coverage.clone()]
                || observations.iter().any(|row| !matches!(&row.fact, OperationalFact::CounterSet(sample) if database_performance_sample_fits_source(sample,source))) {
                return Err(invalid());
            }
            let mut report = captured(&context, coverage, observations, read_complete, true)?;
            let SourceObservation::DatabasePerformanceCounters { native_database, .. } = &source.observation else {
                return Err(invalid());
            };
            report.native_database_id = Some(native_database.id.clone());
            report.database = Some(source.target.database().ok_or_else(invalid)?.database.clone());
            Ok(report)
        }
        PerformanceCounterEvidence::Captured { context, coverage, observations, read_complete } => {
            captured(context, coverage, observations, read_complete, false)
        }
    }
}

fn captured(
    context: &NativeReadContext,
    coverage: &StateSourceCoverage,
    observations: &[&OperationalObservation],
    read_complete: bool,
    database: bool,
) -> Result<PerformanceCounterReport> {
    let profile = context.target.engine().spec().performance_counters().ok_or_else(invalid)?;
    if !context.is_valid()
        || !profile.source_is_valid(coverage)
        || observations.len() > coverage.records as usize
        || read_complete && observations.len() != coverage.records as usize
    {
        return Err(invalid());
    }
    let bounds = ObservationBounds::new(&context.observed_from, &context.observed_until).ok_or_else(invalid)?;
    let samples = observations
        .iter()
        .map(|row| {
            if !row.is_valid() || row.source != coverage.source {
                return Err(invalid());
            }
            let OperationalFact::CounterSet(sample) = &row.fact else { return Err(invalid()) };
            if profile.observed_at(sample)?.is_some_and(|at| !bounds.contains(at))
                || sample.ratios.iter().any(|ratio| ratio.observed_at.as_deref().is_some_and(|at| !bounds.contains(at)))
            {
                return Err(invalid());
            }
            Ok(sample.as_ref())
        })
        .collect::<Result<Vec<_>>>()?;
    let missing =
        !read_complete && matches!(coverage.state, SourceState::Collected | SourceState::Empty | SourceState::Partial);
    let source = SourceOutcome {
        source: coverage.source.clone(),
        state: if missing { SourceState::Partial } else { coverage.state },
        reason: if missing {
            Some(t!("view.performance_counters.limit.read"))
        } else {
            coverage
                .stopped
                .map(|stop| stop.error().message)
                .or_else(|| coverage.error_code.map(|_| t!("view.performance_counters.summary.unavailable")))
        },
        reason_message: None,
    };
    let mut report = render(context.target.engine(), &samples, Some(&source), Some(read_complete), database)?;
    report.context = Some(ObservationReadContext::Native { context: Box::new(context.clone()) });
    report.native_source = Some(coverage.clone());
    report.native_observations = Some(coverage.records);
    if !context.has_qualified_local_owner() {
        report.limitations.push(t!("view.performance_counters.limit.owner"));
    }
    Ok(report)
}

fn render(
    engine: Engine,
    samples: &[&NativeCounterSet],
    source: Option<&SourceOutcome>,
    read_complete: Option<bool>,
    database: bool,
) -> Result<PerformanceCounterReport> {
    let profile = engine.spec().performance_counters().ok_or_else(invalid)?;
    if samples.len() > CollectionBudget::upper_limits().max_snapshot_rows as usize
        || source.is_some_and(|source| source.source != culprit_core::source_id::PERFORMANCE_COUNTERS)
        || !super::source_coverage::consistent_source(
            source,
            samples.iter().map(|_| culprit_core::source_id::PERFORMANCE_COUNTERS),
        )
    {
        return Err(invalid());
    }
    let mut keys = BTreeSet::new();
    let mut withheld = false;
    let mut native_rows = Some(0_u32);
    for sample in samples {
        if !sample.is_valid()
            || sample.scope
                != if database {
                    culprit_core::MeasurementScope::Database
                } else {
                    culprit_core::MeasurementScope::Server
                }
            || !keys.insert(sample.key()?)
        {
            return Err(invalid());
        }
        if sample.properties.get(PERFORMANCE_COUNTER_WITHHELD_PROPERTY).is_some_and(|value| value == "true") {
            withheld = true;
            native_rows = None;
        } else {
            profile.validate(sample)?;
            native_rows =
                native_rows.zip(profile.native_rows(sample)?).and_then(|(total, rows)| total.checked_add(rows));
        }
    }
    // Missing bodies do not establish how many DMV rows the missing aggregate originally contained.
    if samples.is_empty() && source.is_none_or(|source| source.state != SourceState::Empty) {
        native_rows = None;
    }
    let observed = samples.iter().map(|sample| sample.counters.len() + sample.ratio_gaps.len()).sum::<usize>();
    let mut entries = Vec::new();
    for sample in samples {
        if sample.properties.get(PERFORMANCE_COUNTER_WITHHELD_PROPERTY).is_some_and(|value| value == "true") {
            continue;
        }
        entries::append(profile, sample, &mut entries)?;
    }
    let limited = entries.len() < observed && !withheld;
    let partial = read_complete == Some(false) || source.is_some_and(|source| source.state == SourceState::Partial);
    let unavailable =
        source.is_some_and(|source| matches!(source.state, SourceState::Unavailable | SourceState::NotApplicable));
    let mut limitations = vec![
        if database {
            t!("view.performance_counters.limit.database")
        } else {
            t!("view.performance_counters.limit.scope")
        },
        t!("view.performance_counters.limit.lifetime"),
        t!("view.performance_counters.limit.cause"),
    ];
    if !database {
        limitations.push(t!("view.performance_counters.limit.overlap"));
    }
    if read_complete.is_none() {
        limitations.push(t!("view.performance_counters.limit.original"));
        limitations.push(t!("view.performance_counters.limit.owner"));
    }
    if source.is_none() {
        limitations.push(t!("view.performance_counters.limit.coverage"));
    }
    if partial {
        limitations.push(t!("view.performance_counters.limit.read"));
    }
    if limited {
        limitations.push(t!("view.performance_counters.limit.presentation"));
    }
    if withheld {
        limitations.push(t!("view.performance_counters.limit.withheld"));
    }
    if samples.iter().any(|sample| !profile.declares_selection(sample)) {
        limitations.push(t!("view.performance_counters.limit.selection"));
    }
    if samples.iter().any(|sample| !withheld && profile.observed_at(sample).ok().flatten().is_none()) {
        limitations.push(t!("view.performance_counters.limit.clock"));
    }
    let missing_fields = if withheld { Vec::new() } else { entries::missing(profile, samples, database) };
    let ratios = if samples.iter().any(|sample| !sample.ratios.is_empty()) {
        Some(Box::new(super::counter_ratios::explain_borrowed(samples, source, read_complete.unwrap_or(false))?))
    } else {
        None
    };
    Ok(PerformanceCounterReport {
        native_database_id: None,
        database: None,
        context: None,
        native_source: None,
        source_state: source.map(|source| source.state),
        source_reason: source.and_then(|source| source.reason.clone()),
        read_complete,
        native_observations: None,
        observed_observations: samples.len() as u32,
        native_counter_rows: native_rows,
        observed_counter_rows: observed as u32,
        presented_counter_rows: entries.len() as u32,
        entries_limited: limited,
        fields_withheld: withheld,
        missing_fields,
        title: t!("view.performance_counters.title"),
        summary: if unavailable {
            t!("view.performance_counters.summary.unavailable")
        } else if withheld {
            t!("view.performance_counters.limit.withheld")
        } else if partial {
            t!("view.performance_counters.summary.partial")
        } else if entries.is_empty() {
            t!("view.performance_counters.summary.empty")
        } else {
            t!(
                "view.performance_counters.summary.observed",
                observed = format::digits(observed.to_string()),
                presented = format::digits(entries.len().to_string())
            )
        },
        limitations,
        entries,
        ratios,
    })
}
fn invalid() -> Error {
    Error::invalid(t!("view.performance_counters.invalid"))
}

#[cfg(test)]
mod tests;
