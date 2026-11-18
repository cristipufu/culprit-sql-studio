//! Exact native wait inputs remain independent of category summaries, current pressure and query attribution.
use culprit_core::{
    Comparison, ComparisonReason, Engine, Error, MeasurementScope, Metric, ResetMarker, Result, Snapshot,
    SourceOutcome, SourceState, WaitCounterEntry, WaitCounterIssue, WaitCounterReport, format, t,
    telemetry::{ObservationReadContext, StateSourceCoverage, WaitCounterObservation, WaitCounterSnapshot},
};

pub const MAX_WAIT_COUNTER_ENTRIES: usize = 256;

pub fn explain_snapshot(snapshot: &Snapshot) -> Result<Option<WaitCounterReport>> {
    let Some(read) = snapshot.pressure.as_ref().and_then(|pressure| pressure.wait_counters.as_deref()) else {
        return Ok(None);
    };
    let comparison = read
        .context
        .as_ref()
        .and_then(ObservationReadContext::native)
        .map(|context| context.target.snapshot_namespace_comparison(snapshot));
    let unknown = match comparison {
        None | Some(Comparison::Compatible) => false,
        Some(Comparison::Incompatible(ComparisonReason::UnknownIdentity)) => true,
        _ => return Err(invalid()),
    };
    let mut report = explain(read, snapshot.server.engine)?;
    if unknown {
        report.limitations.push(t!("view.observations.limit.namespace"));
    }
    Ok(Some(report))
}

pub fn explain(read: &WaitCounterSnapshot, engine: Engine) -> Result<WaitCounterReport> {
    if !read.is_valid_for(engine) {
        return Err(invalid());
    }
    let entries = read.observations.iter().take(MAX_WAIT_COUNTER_ENTRIES).map(entry).collect::<Vec<_>>();
    let mut counts = std::collections::BTreeMap::<&str, usize>::new();
    for row in &read.observations {
        *counts.entry(&row.source).or_default() += 1;
    }
    let sources = read
        .sources
        .iter()
        .map(|source| outcome(source, counts.get(source.source.as_str()).copied().unwrap_or(0), read.read_complete))
        .collect::<Vec<_>>();
    let partial = !read.read_complete
        || sources.iter().any(|source| {
            matches!(source.state, SourceState::Partial | SourceState::Unavailable | SourceState::NotApplicable)
        });
    let native_rows: u32 = read.sources.iter().map(|source| source.records).sum();
    let entries_limited = entries.len() < read.observations.len();
    let mut limitations = vec![t!("view.wait_counters.limit.scope"), t!("view.wait_counters.limit.cause")];
    limitations.push(match &read.context {
        Some(ObservationReadContext::Collector { .. }) => t!("view.wait_counters.limit.client_clock"),
        Some(ObservationReadContext::Native { .. }) => t!("view.wait_counters.limit.native_clock"),
        None => t!("view.wait_counters.limit.clock_unavailable"),
    });
    if !read.read_complete {
        limitations.push(t!("view.wait_counters.limit.read"));
    }
    if entries_limited {
        limitations.push(t!("view.wait_counters.limit.presentation"));
    }
    Ok(WaitCounterReport {
        context: read.context.clone(),
        sources,
        read_complete: read.read_complete,
        native_rows,
        observed_rows: read.observations.len() as u32,
        presented_rows: entries.len() as u32,
        entries_limited,
        title: t!("view.wait_counters.title"),
        summary: if partial {
            t!("view.wait_counters.summary.partial")
        } else if read.observations.is_empty() {
            t!("view.wait_counters.summary.empty")
        } else {
            t!(
                "view.wait_counters.summary.observed",
                native = format::digits(native_rows.to_string()),
                observed = format::digits(read.observations.len().to_string()),
                presented = format::digits(entries.len().to_string())
            )
        },
        limitations,
        entries,
    })
}

fn entry(row: &WaitCounterObservation) -> WaitCounterEntry {
    let resource = row.resource_wait_time_ms();
    let issues = if resource.is_none() { vec![WaitCounterIssue::SignalExceedsTotal] } else { Vec::new() };
    let mut explanation = t!("view.wait_counters.entry.explanation");
    if let Some(mechanism) = row.mechanism {
        explanation.push(' ');
        explanation.push_str(&t!("view.waits.mechanisms", mechanisms = mechanism.label()));
    }
    if !issues.is_empty() {
        explanation.push(' ');
        explanation.push_str(&t!("view.wait_counters.issue.signal_exceeds_total"));
    }
    let number = |key, label, value: &str| Metric::text(label, format::digits(value.to_owned())).keyed(key);
    let mut metrics = vec![
        number("view.wait_counters.metric.tasks", t!("view.wait_counters.metric.tasks"), &row.waiting_tasks_count),
        number("view.wait_counters.metric.total", t!("view.wait_counters.metric.total"), &row.wait_time_ms),
        number("view.wait_counters.metric.signal", t!("view.wait_counters.metric.signal"), &row.signal_wait_time_ms),
        number("view.wait_counters.metric.maximum", t!("view.wait_counters.metric.maximum"), &row.max_wait_time_ms),
        Metric::text(
            t!("view.wait_counters.metric.resource"),
            resource
                .as_deref()
                .map(|value| format::digits(value.to_owned()))
                .unwrap_or_else(|| t!("view.wait_counters.unknown")),
        )
        .keyed("view.wait_counters.metric.resource"),
        Metric::text(t!("view.wait_counters.metric.scope"), scope(row.scope)).keyed("view.wait_counters.metric.scope"),
    ];
    metrics.push(
        Metric::text(
            t!("view.wait_counters.metric.reset"),
            match &row.reset {
                ResetMarker::At(at) | ResetMarker::Generation(at) => at.clone(),
                ResetMarker::Unknown => t!("view.wait_counters.unknown"),
            },
        )
        .keyed("view.wait_counters.metric.reset"),
    );
    WaitCounterEntry {
        observation: row.clone(),
        resource_wait_time_ms: resource,
        issues,
        title: row.wait_type.clone(),
        explanation,
        metrics,
    }
}

fn scope(scope: MeasurementScope) -> String {
    match scope {
        MeasurementScope::Database => t!("view.wait_counters.scope.database"),
        MeasurementScope::Pool => t!("view.wait_counters.scope.pool"),
        MeasurementScope::Node => t!("view.wait_counters.scope.node"),
        MeasurementScope::Cluster => t!("view.wait_counters.scope.cluster"),
        MeasurementScope::Server => t!("view.wait_counters.scope.server"),
        MeasurementScope::Session => t!("view.wait_counters.scope.session"),
        MeasurementScope::File => t!("view.wait_counters.scope.file"),
        MeasurementScope::Unknown => t!("view.wait_counters.unknown"),
    }
}
fn outcome(source: &StateSourceCoverage, rows: usize, read_complete: bool) -> SourceOutcome {
    let readable = matches!(source.state, SourceState::Collected | SourceState::Empty | SourceState::Partial);
    let missing = readable && (!read_complete || rows != source.records as usize);
    let reason = if missing {
        Some(t!("view.wait_counters.limit.read"))
    } else if let Some(stop) = source.stopped {
        Some(stop.error().message)
    } else if source.error_code.is_some() {
        Some(t!("view.wait_counters.source.unavailable"))
    } else if source.state == SourceState::Partial {
        Some(t!("view.wait_counters.source.partial"))
    } else {
        None
    };
    SourceOutcome {
        source: source.source.clone(),
        state: if missing { SourceState::Partial } else { source.state },
        reason,
        reason_message: None,
    }
}
fn invalid() -> Error {
    Error::invalid(t!("view.wait_counters.invalid"))
}

#[cfg(test)]
mod tests;
