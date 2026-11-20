//! The exact selected catalog population remains independent of index health, pressure and physical-stats claims.
use culprit_core::{
    ColumnstoreRowgroupEntry, ColumnstoreRowgroupReport, Metric, Result, SourceOutcome, SourceState, format, t,
    telemetry::*,
};
use std::collections::BTreeSet;

pub struct ColumnstoreRowgroupEvidence<'a> {
    pub source: &'a CollectionDescription,
    pub observations: &'a [&'a OperationalObservation],
    pub read_complete: bool,
}
pub fn explain_captured(read: &CapturedOperationalRead) -> Result<ColumnstoreRowgroupReport> {
    if !read.is_valid() {
        return Err(invalid());
    }
    explain(ColumnstoreRowgroupEvidence {
        source: &read.description,
        observations: &read.observations.iter().collect::<Vec<_>>(),
        read_complete: read.read_complete,
    })
}
pub fn explain(evidence: ColumnstoreRowgroupEvidence<'_>) -> Result<ColumnstoreRowgroupReport> {
    let context = columnstore_rowgroup_context(evidence.source)?;
    let SourceObservation::ColumnstoreRowgroups { snapshot, target, .. } = &evidence.source.observation else {
        return Err(invalid());
    };
    let [coverage] = snapshot.sources.as_slice() else { return Err(invalid()) };
    let mut keys = BTreeSet::new();
    let mut points = Vec::new();
    for row in evidence.observations {
        let OperationalFact::MssqlColumnstoreRowgroup(point) = &row.fact else { return Err(invalid()) };
        if !row.is_valid()
            || !point.fits_source(evidence.source)
            || !keys.insert(&row.key)
            || !matches!(coverage.state, SourceState::Collected | SourceState::Partial)
        {
            return Err(invalid());
        }
        points.push((*row, point.as_ref()));
    }
    if points.len() > coverage.records as usize || evidence.read_complete && points.len() != coverage.records as usize {
        return Err(invalid());
    }
    let missing = !evidence.read_complete || points.len() != coverage.records as usize;
    let state = if missing {
        if points.is_empty() { SourceState::Unavailable } else { SourceState::Partial }
    } else {
        coverage.state
    };
    let reason = if missing {
        Some(t!("view.columnstore_rowgroups.partial"))
    } else if let Some(stop) = coverage.stopped {
        Some(stop.error().message)
    } else {
        coverage.error_code.map(|_| t!("view.columnstore_rowgroups.unavailable"))
    };
    let mut limitations = vec![
        t!("view.columnstore_rowgroups.limit.point"),
        t!("view.columnstore_rowgroups.limit.bytes"),
        t!("view.columnstore_rowgroups.limit.deleted"),
    ];
    if state == SourceState::Partial || state == SourceState::Unavailable {
        limitations.push(t!("view.columnstore_rowgroups.partial"));
    }
    points.sort_by_key(|(_, point)| (point.partition_number, point.row_group_id));
    let entries = points
        .iter()
        .map(|(row, point)| ColumnstoreRowgroupEntry {
            observation_key: row.key.clone(),
            observed_at: point.observed_at.clone(),
            partition_number: point.partition_number,
            row_group_id: point.row_group_id,
            state: point.state,
            title: t!(
                "view.columnstore_rowgroups.entry",
                partition = format::digits(point.partition_number.to_string()),
                group = format::digits(point.row_group_id.to_string())
            ),
            metrics: vec![
                Metric::said_text(
                    culprit_core::key!("view.columnstore_rowgroups.state"),
                    t!(
                        "view.columnstore_rowgroups.state_value",
                        state = match point.state {
                            0 => "invisible",
                            1 => "open",
                            2 => "closed",
                            3 => "compressed",
                            4 => "tombstone",
                            _ => "unknown",
                        }
                    ),
                ),
                quantity(culprit_core::key!("view.columnstore_rowgroups.rows"), &point.total_rows),
                quantity(culprit_core::key!("view.columnstore_rowgroups.deleted"), &point.deleted_rows),
                quantity(culprit_core::key!("view.columnstore_rowgroups.bytes"), &point.size_bytes),
            ],
        })
        .collect();
    Ok(ColumnstoreRowgroupReport {
        context,
        target: target.as_ref().clone(),
        source: SourceOutcome { source: coverage.source.clone(), state, reason, reason_message: None },
        read_complete: evidence.read_complete,
        native_rows: coverage.records,
        observed_rows: points.len() as u32,
        title: t!("view.columnstore_rowgroups.title"),
        summary: if matches!(state, SourceState::Collected | SourceState::Empty) {
            t!("view.columnstore_rowgroups.summary", count = format::digits(points.len().to_string()))
        } else {
            t!("view.columnstore_rowgroups.unavailable")
        },
        limitations,
        investigation: t!("view.columnstore_rowgroups.investigate"),
        verify: t!("view.columnstore_rowgroups.verify"),
        entries,
    })
}
fn quantity(key: &str, value: &Option<String>) -> Metric {
    Metric::said_text(
        key,
        value
            .as_ref()
            .map(|value| format::digits(value.clone()))
            .unwrap_or_else(|| t!("view.columnstore_rowgroups.unknown")),
    )
}
fn invalid() -> culprit_core::Error {
    CollectionStop::InvalidPage.error()
}

#[cfg(test)]
mod tests;
