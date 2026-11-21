//! Pure original change-feed context. Errors are retained cache rows, never unique events or exclusive causes.
use culprit_core::{
    ChangeCaptureEntry, ChangeCaptureEntryKind, ChangeCaptureField, ChangeFeedReport, ErrorCode, Result, SourceState,
    format, t, telemetry::*,
};
use std::collections::BTreeSet;
pub struct ChangeFeedEvidence<'a> {
    pub source: &'a CollectionDescription,
    pub observations: &'a [&'a OperationalObservation],
    pub read_complete: bool,
}
pub fn explain_captured(read: &CapturedOperationalRead) -> Result<ChangeFeedReport> {
    if !read.is_valid() {
        return Err(CollectionStop::InvalidPage.error());
    }
    explain(ChangeFeedEvidence {
        source: &read.description,
        observations: &read.observations.iter().collect::<Vec<_>>(),
        read_complete: read.read_complete,
    })
}
pub fn explain(evidence: ChangeFeedEvidence<'_>) -> Result<ChangeFeedReport> {
    let qualified = super::change_capture::qualify(
        MssqlChangeCaptureFamily::ChangeFeed,
        evidence.source,
        evidence.observations,
        evidence.read_complete,
    )?;
    let points = qualified
        .points
        .into_iter()
        .map(|(row, point)| {
            let MssqlChangeCapturePoint::ChangeFeed(point) = point else {
                unreachable!("qualified change-feed family")
            };
            (row, point)
        })
        .collect::<Vec<_>>();
    let complete =
        qualified.sources.iter().all(|source| matches!(source.state, SourceState::Collected | SourceState::Empty));
    let source_state = if !evidence.read_complete || !complete {
        if points.is_empty() { SourceState::Unavailable } else { SourceState::Partial }
    } else {
        SourceState::Collected
    };
    let errors = points.iter().filter(|(_, point)| matches!(point.data, MssqlChangeFeedData::Error(_))).count();
    let summary = if errors > 0 {
        t!("view.change_feed.summary.errors", count = format::digits(errors.to_string()))
    } else if source_state == SourceState::Collected {
        t!("view.change_feed.summary.recorded")
    } else {
        t!("view.change_feed.summary.unavailable")
    };
    let mut limitations = vec![
        t!("view.change_feed.limit.history"),
        t!("view.change_feed.limit.reset"),
        t!("view.change_feed.limit.clocks"),
        t!("view.change_feed.limit.delivery"),
    ];
    if !evidence.read_complete || source_state == SourceState::Partial {
        limitations.push(t!("view.change_feed.limit.partial"));
    }
    for source in &qualified.sources {
        if let Some(stop) = source.stopped {
            limitations.push(stop.error().message)
        } else if let Some(code) = source.error_code {
            limitations.push(match code {
                ErrorCode::PermissionDenied => t!("view.change_feed.limit.permission"),
                ErrorCode::Unsupported => t!("view.change_feed.limit.unsupported"),
                _ => t!("view.change_feed.limit.failed"),
            })
        }
    }
    let mut unique = BTreeSet::new();
    limitations.retain(|value| unique.insert(value.clone()));
    let entries = points
        .iter()
        .filter(|(_, p)| matches!(p.data, MssqlChangeFeedData::Error(_)))
        .chain(points.iter().filter(|(_, p)| matches!(p.data, MssqlChangeFeedData::ScanSession(_))))
        .map(|(row, point)| entry(row, point))
        .collect();
    Ok(ChangeFeedReport {
        context: Some(qualified.context),
        sources: qualified.sources,
        source_state,
        read_complete: evidence.read_complete,
        title: t!("view.change_feed.title"),
        summary,
        limitations,
        entries,
    })
}
fn entry(row: &OperationalObservation, point: &MssqlChangeFeedObservation) -> ChangeCaptureEntry {
    let (kind, title, explanation, fields, investigation, verify) = match &point.data {
        MssqlChangeFeedData::Error(error) => (
            ChangeCaptureEntryKind::Error,
            t!("view.change_feed.entry.error", number = format::digits(error.error_number.to_string())),
            t!("view.change_feed.entry.error_context"),
            vec![
                field("session_id", t!("view.change_feed.field.scan_id"), Some(error.session_id.to_string())),
                field("source_task", t!("view.change_feed.field.task"), error.source_task.map(|v| v.to_string())),
                field("table_id", t!("view.change_feed.field.table"), error.table_id.map(|v| v.to_string())),
                field("table_group_id", t!("view.change_feed.field.table_group"), error.table_group_id.clone()),
                field(
                    "capture_phase_number",
                    t!("view.change_feed.field.phase"),
                    error.capture_phase_number.map(|v| v.to_string()),
                ),
                field("error_severity", t!("view.change_feed.field.severity"), Some(error.severity.to_string())),
                field("error_state", t!("view.change_feed.field.state"), Some(error.state.to_string())),
                field("entry_time", t!("view.change_feed.field.entered"), error.entered_without_offset.clone()),
                field("command_id", t!("view.change_feed.field.command"), error.command_id.map(|v| v.to_string())),
            ],
            t!("view.change_feed.error.investigate"),
            t!("view.change_feed.error.verify"),
        ),
        MssqlChangeFeedData::ScanSession(scan) => {
            let aggregate = scan.session_id == 0;
            (
                if aggregate { ChangeCaptureEntryKind::Aggregate } else { ChangeCaptureEntryKind::ScanSession },
                if aggregate {
                    t!("view.change_feed.entry.aggregate")
                } else {
                    t!("view.change_feed.entry.scan", id = format::digits(scan.session_id.to_string()))
                },
                if aggregate {
                    t!("view.change_feed.entry.aggregate_context")
                } else {
                    t!("view.change_feed.entry.scan_context")
                },
                vec![
                    field("batch_processing_phase", t!("view.change_feed.field.phase"), scan.native_phase.clone()),
                    field("start_time", t!("view.change_feed.field.started"), scan.started_without_offset.clone()),
                    field("end_time", t!("view.change_feed.field.ended"), scan.ended_without_offset.clone()),
                    field("tran_count", t!("view.change_feed.field.transactions"), scan.transactions.clone()),
                    field("error_count", t!("view.change_feed.field.errors"), scan.error_count.clone()),
                    field("latency", t!("view.change_feed.field.latency"), scan.latency_seconds.clone()),
                    field(
                        "schema_change_count",
                        t!("view.change_feed.field.schema_changes"),
                        scan.schema_changes.clone(),
                    ),
                    field("empty_scan_count", t!("view.change_feed.field.empty_scans"), scan.empty_scans.clone()),
                    field("duration", t!("view.change_feed.field.duration"), scan.duration_seconds.clone()),
                    field("log_record_count", t!("view.change_feed.field.records"), scan.log_records.clone()),
                    field("command_count", t!("view.change_feed.field.commands"), scan.commands.clone()),
                    field(
                        "failed_sessions_count",
                        t!("view.change_feed.field.failed_scans"),
                        scan.failed_sessions.clone(),
                    ),
                    field(
                        "currently_processed_commit_time",
                        t!("view.change_feed.field.commit"),
                        scan.processed_commit_without_offset.clone(),
                    ),
                ],
                t!("view.change_feed.scan.investigate"),
                t!("view.change_feed.scan.verify"),
            )
        }
        MssqlChangeFeedData::Availability(_) => unreachable!("availability is not an incident entry"),
    };
    ChangeCaptureEntry {
        source: row.source.clone(),
        observation_key: row.key.clone(),
        kind,
        observed_at: point.observed_at.clone(),
        title,
        explanation,
        fields,
        investigation,
        verify,
    }
}
fn field(key: &str, label: String, value: Option<String>) -> ChangeCaptureField {
    ChangeCaptureField { key: key.into(), label, value }
}
#[cfg(test)]
mod tests;

pub fn explain_recorded(recorded: &RecordedOperationalRead) -> Result<ChangeFeedReport> {
    match recorded {
        RecordedOperationalRead::Captured { read } => explain_captured(read),
        RecordedOperationalRead::Unavailable { reason } => Ok(unavailable(reason)),
    }
}
pub fn unavailable(reason: &culprit_core::i18n::Message) -> ChangeFeedReport {
    ChangeFeedReport {
        context: None,
        sources: vec![],
        source_state: SourceState::Unavailable,
        read_complete: false,
        title: t!("view.change_feed.title"),
        summary: reason.render(),
        limitations: vec![t!("view.change_capture.history.original")],
        entries: vec![],
    }
}
