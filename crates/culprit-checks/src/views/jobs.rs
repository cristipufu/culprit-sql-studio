//! Native scheduler points and retained run outcomes share one explanation, without historical ownership joins.
use culprit_core::{
    Error, JobEntry, JobEntryKind, JobsReport, Metric, Result, SourceOutcome, SourceState, format, t, telemetry::*,
};
use std::collections::BTreeMap;
mod agent;
mod continuous_aggregates;
mod points;
const MAX_JOB_ENTRIES: usize = 256;

/// Ordinary scans may display independent native positives under unknown identity only with an explicit limit.
/// A known namespace conflict cannot borrow the scan's database; archive explanation remains source-owned.
pub fn explain_snapshot(snapshot: &culprit_core::Snapshot) -> Result<Option<JobsReport>> {
    let Some(read) = snapshot.operations.jobs.as_deref() else {
        return Ok(None);
    };
    match read.context().target.snapshot_namespace_comparison(snapshot) {
        culprit_core::Comparison::Compatible => explain(read).map(Some),
        culprit_core::Comparison::Incompatible(culprit_core::ComparisonReason::UnknownIdentity) => {
            explain(read).map(|mut report| {
                report.limitations.push(t!("view.observations.limit.namespace"));
                Some(report)
            })
        }
        _ => Err(Error::invalid(t!("view.jobs.invalid"))),
    }
}

pub fn explain(snapshot: &JobsSnapshot) -> Result<JobsReport> {
    explain_bounded(snapshot, MAX_JOB_ENTRIES)
}
pub fn explain_bounded(snapshot: &JobsSnapshot, max_entries: usize) -> Result<JobsReport> {
    if !snapshot.is_valid() {
        return Err(Error::invalid(t!("view.jobs.invalid")));
    }
    match snapshot {
        JobsSnapshot::Timescale { snapshot, read_complete } => {
            explain_timescale_bounded(snapshot, *read_complete, max_entries)
        }
        JobsSnapshot::MssqlAgent { snapshot, read_complete } => agent::explain(snapshot, *read_complete, max_entries),
    }
}
fn explain_timescale_bounded(
    snapshot: &TimescaleJobsSnapshot,
    read_complete: bool,
    max_entries: usize,
) -> Result<JobsReport> {
    if !snapshot.is_valid_read(read_complete) {
        return Err(Error::invalid(t!("view.jobs.invalid")));
    }
    let mut state_ids = BTreeMap::<&str, usize>::new();
    let mut counts = BTreeMap::<&str, usize>::new();
    let mut selected = snapshot
        .observations
        .iter()
        .map(|row| {
            *counts.entry(&row.source).or_default() += 1;
            if let OperationalFact::TimescaleJobState(point) = &row.fact {
                *state_ids.entry(&point.job_id).or_default() += 1;
            }
            (points::priority(&row.fact), &row.source, &row.key, row)
        })
        .collect::<Vec<_>>();
    selected.sort_by(|left, right| (left.0, left.1, left.2).cmp(&(right.0, right.1, right.2)));
    let max_entries = max_entries.min(MAX_JOB_ENTRIES);
    let entries_limited = selected.len() > max_entries;
    let entries = selected
        .into_iter()
        .take(max_entries)
        .map(|(_, _, _, row)| match &row.fact {
            OperationalFact::TimescaleJobState(point) => {
                Ok(points::state(row, point, state_ids.get(point.job_id.as_str()).copied().unwrap_or(0) > 1))
            }
            OperationalFact::TimescaleJobRun(point) => Ok(points::run(row, point)),
            OperationalFact::TimescaleContinuousAggregate(point) => Ok(continuous_aggregates::entry(row, point)),
            _ => Err(Error::invalid(t!("view.jobs.invalid"))),
        })
        .collect::<Result<Vec<_>>>()?;
    let sources = snapshot
        .sources
        .iter()
        .map(|source| outcome(source, counts.get(source.source.as_str()).copied().unwrap_or(0), read_complete))
        .collect();
    let mut limitations = vec![
        t!("view.jobs.limit.point"),
        t!("view.jobs.limit.history"),
        t!("view.jobs.limit.worker"),
        t!("view.jobs.limit.labels"),
        t!("view.jobs.limit.schedule"),
    ];
    if snapshot.metadata.run_visibility != TimescaleJobRunVisibility::DatabaseOwner {
        limitations.push(t!("view.jobs.limit.visibility"));
    }
    if snapshot.metadata.successful_runs_logged != Some(true) {
        limitations.push(t!("view.jobs.limit.logging"));
    }
    if snapshot.metadata.available(TimescaleJobViewKind::ContinuousAggregates) {
        limitations.push(t!("view.jobs.continuous_aggregates.limit"));
    }
    if !snapshot.metadata.available(TimescaleJobViewKind::Statistics) {
        limitations.push(t!("view.jobs.limit.statistics"));
    }
    if !read_complete {
        limitations.push(t!("view.jobs.limit.read"));
    }
    if entries_limited {
        limitations.push(t!("view.jobs.limit.presentation"));
    }
    if state_ids.values().any(|count| *count > 1) {
        limitations.push(t!("view.jobs.limit.duplicates"));
    }
    let has_aggregate_configuration = counts.contains_key(TIMESCALE_CONTINUOUS_AGGREGATES_SOURCE);
    Ok(JobsReport {
        context: snapshot.context.clone(),
        sources,
        read_complete,
        native_rows: snapshot.sources.iter().map(|source| source.records).sum(),
        observed_rows: snapshot.observations.len() as u32,
        entries_limited,
        title: if has_aggregate_configuration {
            t!("view.jobs.continuous_aggregates.report_title")
        } else {
            t!("view.jobs.title")
        },
        summary: if snapshot.metadata.extension.is_none() {
            t!("view.jobs.summary.not_installed")
        } else if snapshot.observations.is_empty() {
            t!("view.jobs.summary.empty")
        } else if has_aggregate_configuration {
            t!("view.jobs.continuous_aggregates.summary")
        } else {
            t!(
                "view.jobs.summary.rows",
                n = snapshot.observations.len(),
                count = format::digits(snapshot.observations.len().to_string())
            )
        },
        limitations,
        entries,
    })
}
fn outcome(source: &StateSourceCoverage, rows: usize, read_complete: bool) -> SourceOutcome {
    let readable = matches!(source.state, SourceState::Collected | SourceState::Empty | SourceState::Partial);
    let missing = readable && (!read_complete || rows != source.records as usize);
    let reason = if missing {
        Some(t!("view.jobs.limit.read"))
    } else if let Some(stop) = source.stopped {
        Some(stop.error().message)
    } else if source.error_code.is_some() {
        Some(t!("view.jobs.source.unavailable"))
    } else if source.state == SourceState::Partial {
        Some(t!("view.jobs.source.partial"))
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
fn entry(
    row: &OperationalObservation,
    job_id: &str,
    kind: JobEntryKind,
    title: String,
    explanation: String,
    metrics: Vec<Metric>,
) -> JobEntry {
    JobEntry {
        source: row.source.clone(),
        observation_key: row.key.clone(),
        job_id: Some(job_id.into()),
        kind,
        title,
        explanation,
        metrics,
        investigation: t!("view.jobs.investigate"),
        verify: t!("view.jobs.verify"),
    }
}
fn timestamp(point: &TimescaleJobTimestamp) -> String {
    match point {
        TimescaleJobTimestamp::Finite { at } => at.clone(),
        TimescaleJobTimestamp::NegativeInfinity => "-infinity".into(),
        TimescaleJobTimestamp::PositiveInfinity => "infinity".into(),
    }
}
#[cfg(test)]
mod tests;
