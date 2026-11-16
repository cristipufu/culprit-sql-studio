//! Native replication states are interpreted once, without inventing application ownership or catch-up time.
use culprit_core::{
    Error, ReplicationCondition, ReplicationEntry, ReplicationReport, Result, SourceOutcome, SourceState, format, t,
    telemetry::*,
};

mod mssql;
mod mysql;
mod postgres;
mod support;

pub const MAX_REPLICATION_REPORT_ENTRIES: usize = 256;

pub fn explain(snapshot: &ReplicationSnapshot, read_complete: bool) -> Result<ReplicationReport> {
    explain_bounded(snapshot, read_complete, MAX_REPLICATION_REPORT_ENTRIES)
}

/// Presentation limits do not restrict native-state examination or change the original source/read counts.
pub fn explain_bounded(
    snapshot: &ReplicationSnapshot,
    read_complete: bool,
    max_entries: usize,
) -> Result<ReplicationReport> {
    if !snapshot.is_valid_read(read_complete) {
        return Err(Error::invalid(t!("view.replication.invalid")));
    }
    let mut counts = std::collections::BTreeMap::<&str, usize>::new();
    let mut selected = snapshot
        .observations
        .iter()
        .map(|row| {
            *counts.entry(&row.source).or_default() += 1;
            (conditions(row), row.source.as_str(), row.fact.key(), row)
        })
        .collect::<Vec<_>>();
    let state_rows = selected.iter().filter(|(conditions, ..)| !conditions.is_empty()).count();
    selected.sort_by(|left, right| (left.0.is_empty(), left.1, &left.2).cmp(&(right.0.is_empty(), right.1, &right.2)));
    let max_entries = max_entries.min(MAX_REPLICATION_REPORT_ENTRIES);
    let entries_limited = selected.len() > max_entries;
    let entries = selected
        .into_iter()
        .take(max_entries)
        .map(|(conditions, _, _, row)| {
            let mut entry = render(row);
            entry.conditions = conditions;
            entry
        })
        .collect::<Vec<_>>();
    let sources = snapshot
        .sources
        .iter()
        .map(|source| outcome(source, counts.get(source.source.as_str()).copied().unwrap_or(0), read_complete))
        .collect();
    let mut limitations = vec![
        t!("view.replication.limit.point"),
        t!("view.replication.limit.owner"),
        t!("view.replication.limit.timing"),
        t!("view.replication.limit.topology"),
    ];
    if snapshot.context.native().is_none() {
        limitations.push(t!("view.replication.limit.collector_clock"));
    } else if snapshot.context.native().is_some_and(|context| !context.has_qualified_local_owner()) {
        limitations.push(t!("view.replication.limit.origin"));
    }
    if !read_complete {
        limitations.push(t!("view.replication.limit.read"));
    }
    if snapshot.sources.iter().any(|source| !source.complete_scope) {
        limitations.push(t!("view.replication.limit.source"));
    }
    if entries_limited {
        limitations.push(t!("view.replication.limit.presentation"));
    }
    Ok(ReplicationReport {
        context: snapshot.context.clone(),
        sources,
        read_complete,
        native_rows: snapshot.sources.iter().map(|source| source.records).sum(),
        observed_rows: snapshot.observations.len() as u32,
        state_rows: state_rows as u32,
        presented_rows: entries.len() as u32,
        entries_limited,
        title: t!("view.replication.title"),
        summary: t!(
            "view.replication.summary",
            n = snapshot.observations.len(),
            count = format::digits(snapshot.observations.len().to_string()),
            shown = format::digits(entries.len().to_string())
        ),
        limitations,
        entries,
    })
}

fn conditions(row: &ReplicationObservation) -> Vec<ReplicationCondition> {
    match &row.fact {
        ReplicationFact::AvailabilityGroup(sample) => mssql::availability_conditions(sample),
        ReplicationFact::GeoLink(sample) => mssql::geo_conditions(sample),
        ReplicationFact::Native(sample) => {
            postgres::conditions(row, sample).or_else(|| mysql::conditions(row, sample)).unwrap_or_default()
        }
    }
}

fn render(row: &ReplicationObservation) -> ReplicationEntry {
    match &row.fact {
        ReplicationFact::AvailabilityGroup(sample) => mssql::availability(row, sample),
        ReplicationFact::GeoLink(sample) => mssql::geo(row, sample),
        ReplicationFact::Native(sample) => {
            postgres::explain(row, sample).or_else(|| mysql::explain(row, sample)).unwrap_or_else(|| {
                support::entry(
                    row,
                    sample.measurements.scope,
                    t!("view.replication.unknown.title"),
                    t!("view.replication.unknown.detail"),
                )
            })
        }
    }
}

fn outcome(source: &StateSourceCoverage, rows: usize, read_complete: bool) -> SourceOutcome {
    let readable = matches!(source.state, SourceState::Collected | SourceState::Partial | SourceState::Empty);
    let incomplete = readable && (!read_complete || rows != source.records as usize);
    let reason = if incomplete {
        Some(t!("view.replication.limit.read"))
    } else if source.error_code.is_some() {
        Some(t!("view.replication.source_error"))
    } else {
        None
    };
    SourceOutcome {
        source: source.source.clone(),
        state: if incomplete { SourceState::Partial } else { source.state },
        reason,
        reason_message: None,
    }
}

#[cfg(test)]
mod tests;
