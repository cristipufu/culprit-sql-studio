//! One interpretation of the exact original runtime roster. Empty/partial rows never establish index health.
use culprit_core::{
    FullTextPopulationEntry, FullTextPopulationOrigin, FullTextPopulationReport, Metric, Result, SourceOutcome,
    SourceState, format, t, telemetry::*,
};
use std::collections::BTreeSet;

pub struct FullTextPopulationEvidence<'a> {
    pub source: &'a CollectionDescription,
    pub observations: &'a [&'a OperationalObservation],
    pub read_complete: bool,
}
pub fn explain_captured(read: &CapturedOperationalRead) -> Result<FullTextPopulationReport> {
    if !read.is_valid() {
        return Err(invalid());
    }
    explain(FullTextPopulationEvidence {
        source: &read.description,
        observations: &read.observations.iter().collect::<Vec<_>>(),
        read_complete: read.read_complete,
    })
}
pub fn explain(evidence: FullTextPopulationEvidence<'_>) -> Result<FullTextPopulationReport> {
    let context = full_text_population_context(evidence.source)?;
    let SourceObservation::FullTextPopulations { snapshot, target, metadata, .. } = &evidence.source.observation else {
        return Err(invalid());
    };
    let [coverage] = snapshot.sources.as_slice() else {
        return Err(invalid());
    };
    let mut keys = BTreeSet::new();
    let mut positions = BTreeSet::new();
    let mut points = Vec::new();
    for row in evidence.observations {
        let OperationalFact::MssqlFullTextPopulation(point) = &row.fact else {
            return Err(invalid());
        };
        if !row.is_valid()
            || !point.fits_source(evidence.source)
            || !keys.insert(&row.key)
            || !positions.insert(point.position)
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
        Some(t!("view.full_text_populations.partial"))
    } else if let Some(stop) = coverage.stopped {
        Some(stop.error().message)
    } else {
        coverage.error_code.map(|code| {
            t!(
                "view.full_text_populations.unavailable_reason",
                reason = match code {
                    culprit_core::ErrorCode::PermissionDenied => "permission",
                    culprit_core::ErrorCode::Unsupported => "unsupported",
                    _ => "read",
                }
            )
        })
    };
    let mut limitations = vec![
        t!("view.full_text_populations.limit.point"),
        t!("view.full_text_populations.limit.identity"),
        t!("view.full_text_populations.limit.time"),
    ];
    if matches!(state, SourceState::Partial | SourceState::Unavailable) {
        limitations.push(t!("view.full_text_populations.partial"));
    }
    points.sort_by_key(|(_, point)| point.position);
    let entries = points
        .iter()
        .map(|(row, point)| FullTextPopulationEntry {
            observation_key: row.key.clone(),
            position: point.position,
            observed_at: point.observed_at.clone(),
            status: point.status,
            summary: t!(
                "view.full_text_populations.status",
                state = match point.status {
                    Some(3) => "starting",
                    Some(5) => "processing",
                    Some(7) => "stopped",
                    Some(11) => "aborted",
                    Some(12) => "semantic",
                    _ => "unknown",
                }
            ),
            metrics: vec![
                native_pair(
                    culprit_core::key!("view.full_text_populations.status_code"),
                    point.status,
                    &point.status_description,
                ),
                native_pair(
                    culprit_core::key!("view.full_text_populations.population_type"),
                    point.population_type,
                    &point.population_type_description,
                ),
                native_pair(
                    culprit_core::key!("view.full_text_populations.completion_type"),
                    point.completion_type,
                    &point.completion_type_description,
                ),
                native_pair(
                    culprit_core::key!("view.full_text_populations.queued_type"),
                    point.queued_population_type,
                    &point.queued_population_type_description,
                ),
                quantity(culprit_core::key!("view.full_text_populations.ranges"), point.range_count),
                quantity(
                    culprit_core::key!("view.full_text_populations.completed_ranges"),
                    point.completed_range_count,
                ),
                quantity(
                    culprit_core::key!("view.full_text_populations.outstanding_batches"),
                    point.outstanding_batch_count,
                ),
                Metric::said_text(
                    culprit_core::key!("view.full_text_populations.started_local"),
                    point.started_without_offset.clone().unwrap_or_else(|| t!("view.full_text_populations.unknown")),
                ),
                Metric::said_text(
                    culprit_core::key!("view.full_text_populations.clustered_scan"),
                    t!(
                        "view.full_text_populations.boolean",
                        state = match point.clustered_index_scan {
                            Some(true) => "yes",
                            Some(false) => "no",
                            None => "unknown",
                        }
                    ),
                ),
            ],
        })
        .collect();
    Ok(FullTextPopulationReport {
        original: Some(FullTextPopulationOrigin {
            metadata: metadata.as_ref().clone(),
            context,
            target: target.as_ref().clone(),
        }),
        source: SourceOutcome { source: coverage.source.clone(), state, reason, reason_message: None },
        read_complete: evidence.read_complete,
        native_rows: Some(coverage.records),
        observed_rows: points.len() as u32,
        title: t!("view.full_text_populations.title"),
        summary: if state == SourceState::Partial {
            t!("view.full_text_populations.partial_summary", count = format::digits(points.len().to_string()))
        } else if !matches!(state, SourceState::Collected | SourceState::Empty) {
            t!("view.full_text_populations.unavailable")
        } else if points.is_empty() {
            t!("view.full_text_populations.empty")
        } else {
            t!("view.full_text_populations.summary", count = format::digits(points.len().to_string()))
        },
        limitations,
        investigation: t!("view.full_text_populations.investigate"),
        verify: t!("view.full_text_populations.verify"),
        entries,
    })
}
pub fn explain_recorded(recorded: &RecordedOperationalRead) -> Result<FullTextPopulationReport> {
    match recorded {
        RecordedOperationalRead::Captured { read } => explain_captured(read),
        RecordedOperationalRead::Unavailable { reason } => Ok(unavailable(reason)),
    }
}
pub fn unavailable(reason: &culprit_core::i18n::Message) -> FullTextPopulationReport {
    FullTextPopulationReport {
        original: None,
        source: SourceOutcome {
            source: MSSQL_FULL_TEXT_POPULATIONS_SOURCE.into(),
            state: SourceState::Unavailable,
            reason: Some(reason.render()),
            reason_message: Some(reason.clone()),
        },
        read_complete: false,
        native_rows: None,
        observed_rows: 0,
        title: t!("view.full_text_populations.title"),
        summary: reason.render(),
        limitations: vec![
            t!("view.full_text_populations.limit.point"),
            t!("view.full_text_populations.limit.identity"),
            t!("view.full_text_populations.limit.time"),
        ],
        investigation: t!("view.full_text_populations.investigate"),
        verify: t!("view.full_text_populations.verify"),
        entries: Vec::new(),
    }
}
fn native_pair(key: &str, code: Option<i32>, description: &Option<String>) -> Metric {
    Metric::said_text(
        key,
        t!(
            "view.full_text_populations.code_description",
            code = code
                .map(|value| format::digits(value.to_string()))
                .unwrap_or_else(|| t!("view.full_text_populations.unknown")),
            description = description.clone().unwrap_or_else(|| t!("view.full_text_populations.unknown"))
        ),
    )
}
fn quantity(key: &str, value: Option<u32>) -> Metric {
    Metric::said_text(
        key,
        value
            .map(|value| format::digits(value.to_string()))
            .unwrap_or_else(|| t!("view.full_text_populations.unknown")),
    )
}
fn invalid() -> culprit_core::Error {
    CollectionStop::InvalidPage.error()
}
#[cfg(test)]
mod tests;
