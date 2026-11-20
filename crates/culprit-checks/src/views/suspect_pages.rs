//! One informational interpretation for scan facts and independently authenticated native originals.
use culprit_core::{Metric, Result, Snapshot, SourceState, SuspectPageHistoryReport, format, t, telemetry::*};

pub fn explain_snapshot(snapshot: &Snapshot) -> Result<Option<SuspectPageHistoryReport>> {
    if let Some(read) = snapshot.operations.suspect_pages.as_deref() {
        if snapshot.server.engine != culprit_core::Engine::Mssql
            || read.target.database().is_none_or(|database| database.database != snapshot.server.database)
            || (read.observation.is_some()
                && snapshot.sources.iter().any(|source| {
                    source.source == culprit_core::source_id::SUSPECT_PAGES && source.state == SourceState::Unavailable
                }))
        {
            return Err(CollectionStop::InvalidPage.error());
        }
        return explain(read).map(Some);
    }
    let Some(source) = snapshot.sources.iter().find(|source| source.source == culprit_core::source_id::SUSPECT_PAGES)
    else {
        return Ok(None);
    };
    let mut report = unavailable();
    report.source_state = Some(match source.state {
        SourceState::Collected | SourceState::Partial | SourceState::Empty => SourceState::Partial,
        state => state,
    });
    report.source_reason = source.reason.clone();
    Ok(Some(report))
}

pub fn explain_captured(read: &CapturedOperationalRead) -> Result<SuspectPageHistoryReport> {
    if !read.is_valid() {
        return Err(CollectionStop::InvalidPage.error());
    }
    explain(&SuspectPageHistoryRead::from_source(
        &read.description,
        &read.observations.iter().collect::<Vec<_>>(),
        read.read_complete,
    )?)
}

pub fn explain(read: &SuspectPageHistoryRead) -> Result<SuspectPageHistoryReport> {
    if !read.is_valid() {
        return Err(CollectionStop::InvalidPage.error());
    }
    let mut report = unavailable();
    report.context = Some(read.context.clone());
    report.native_source = Some(read.source.clone());
    report.source_state =
        Some(if !read.read_complete && matches!(read.source.state, SourceState::Collected | SourceState::Partial) {
            SourceState::Partial
        } else {
            read.source.state
        });
    report.read_complete = Some(read.read_complete);
    report.source_reason = match read.source.error_code {
        Some(culprit_core::ErrorCode::PermissionDenied) => Some(t!("view.suspect_pages.permission")),
        Some(culprit_core::ErrorCode::Unsupported) => Some(t!("view.suspect_pages.unsupported")),
        code => read
            .source
            .stopped
            .map(|stop| stop.error().message)
            .or_else(|| code.map(|_| t!("view.suspect_pages.read_failed"))),
    };
    if !read.read_complete || read.source.state == SourceState::Partial {
        report.limitations.push(t!("view.suspect_pages.limit.read"));
    }
    let Some(point) = read.observation.as_deref() else {
        return Ok(report);
    };
    report.database = Some(point.database.clone());
    report.native_database_id = Some(point.native_database_id.clone());
    report.observed_at = Some(point.observed_at.clone());
    report.active_rows = Some(point.count.clone());
    report.retained_rows = Some(point.retained_rows.clone());
    report.last_reported_time_without_offset = point.last_reported_time_without_offset.clone();
    report.summary = t!(
        "view.suspect_pages.recorded",
        retained = format::digits(point.retained_rows.clone()),
        active = format::digits(point.count.clone())
    );
    report.metrics = vec![
        Metric::said_text(
            culprit_core::key!("view.suspect_pages.retained"),
            format::digits(point.retained_rows.clone()),
        ),
        Metric::said_text(culprit_core::key!("view.suspect_pages.active"), format::digits(point.count.clone())),
    ];
    Ok(report)
}

fn unavailable() -> SuspectPageHistoryReport {
    SuspectPageHistoryReport {
        context: None,
        native_source: None,
        source_state: Some(SourceState::Unavailable),
        source_reason: None,
        read_complete: None,
        database: None,
        native_database_id: None,
        observed_at: None,
        active_rows: None,
        retained_rows: None,
        last_reported_time_without_offset: None,
        title: t!("view.suspect_pages.title"),
        summary: t!("view.suspect_pages.unavailable"),
        metrics: vec![],
        limitations: vec![
            t!("view.suspect_pages.limit.scope"),
            t!("view.suspect_pages.limit.incarnation"),
            t!("view.suspect_pages.limit.editable"),
            t!("view.suspect_pages.limit.capacity"),
            t!("view.suspect_pages.limit.zero"),
            t!("view.suspect_pages.limit.time"),
        ],
    }
}
#[cfg(test)]
mod tests;
