//! Configured events and native last starts remain independent from execution outcomes.
use super::*;
pub(super) fn explain(read: &MysqlEventsSnapshot, read_complete: bool, max_entries: usize) -> Result<JobsReport> {
    if !read.is_valid_read(read_complete) {
        return Err(invalid());
    }
    let mut rows = read.observations.iter().collect::<Vec<_>>();
    rows.sort_by_key(|row| &row.key);
    let limit = max_entries.min(MAX_JOB_ENTRIES);
    let entries = rows.iter().take(limit).map(|row| entry(row)).collect::<Result<Vec<_>>>()?;
    let coverage = &read.sources[0];
    let mut limitations = vec![
        t!("view.mysql_events.limit.outcome"),
        t!("view.mysql_events.limit.scope"),
        t!("view.mysql_events.limit.clock"),
        t!("view.mysql_events.limit.setting"),
    ];
    match read.sources[0].error_code {
        Some(culprit_core::ErrorCode::PermissionDenied) => limitations.push(t!("view.mysql_events.limit.denied")),
        Some(culprit_core::ErrorCode::Unsupported) => limitations.push(t!("view.mysql_events.limit.unsupported")),
        Some(_) => limitations.push(t!("view.mysql_events.limit.failed")),
        None => {}
    }
    if !read.metadata.database_event_visibility {
        limitations.push(t!("view.mysql_events.limit.visibility"));
    }
    if !read_complete {
        limitations.push(t!("view.jobs.limit.read"));
    }
    if read.sources.iter().any(|source| source.state == SourceState::Partial) {
        limitations.push(t!("view.mysql_events.limit.partial"));
    }
    if rows.len() > limit {
        limitations.push(t!("view.jobs.limit.presentation"));
    }
    Ok(JobsReport {
        context: read.context.clone(),
        sources: vec![outcome(coverage, rows.len(), read_complete)],
        read_complete,
        native_rows: coverage.records,
        observed_rows: rows.len() as u32,
        entries_limited: rows.len() > limit,
        title: t!("view.mysql_events.title"),
        summary: t!(
            "view.mysql_events.summary",
            setting = setting(read.metadata.scheduler_setting),
            at = &read.metadata.scheduler_observed_at,
            zone = read.metadata.session_time_zone.as_deref().unwrap_or(&t!("view.mysql_events.unknown"))
        ),
        limitations,
        entries,
    })
}
fn setting(setting: Option<MysqlEventSchedulerSetting>) -> String {
    match setting {
        Some(MysqlEventSchedulerSetting::On) => "ON".into(),
        Some(MysqlEventSchedulerSetting::Off) => "OFF".into(),
        Some(MysqlEventSchedulerSetting::Disabled) => "DISABLED".into(),
        _ => t!("view.mysql_events.unknown"),
    }
}
fn entry(row: &OperationalObservation) -> Result<JobEntry> {
    let OperationalFact::MysqlScheduledEvent(event) = &row.fact else { return Err(invalid()) };
    let unknown = t!("view.mysql_events.unknown");
    let event_type = match event.event_type {
        Some(MysqlEventType::OneTime) => t!("view.mysql_events.type.one_time"),
        Some(MysqlEventType::Recurring) => t!("view.mysql_events.type.recurring"),
        _ => unknown.clone(),
    };
    let mut metrics = vec![
        Metric::said_text(culprit_core::key!("view.mysql_events.field.type"), event_type),
        Metric::said_text(culprit_core::key!("view.mysql_events.field.name"), &event.name),
        Metric::said_text(culprit_core::key!("view.mysql_events.field.schema"), &event.schema),
        Metric::said_text(culprit_core::key!("view.mysql_events.field.observed_at"), &event.observed_at),
    ];
    for (key, value) in [
        (culprit_core::key!("view.mysql_events.field.time_zone"), event.time_zone.as_deref()),
        (culprit_core::key!("view.mysql_events.field.interval_value"), event.interval_value.as_deref()),
        (culprit_core::key!("view.mysql_events.field.interval_field"), event.interval_field.as_deref()),
        (culprit_core::key!("view.mysql_events.field.execute_at"), event.execute_at_without_offset.as_deref()),
        (culprit_core::key!("view.mysql_events.field.starts"), event.starts_without_offset.as_deref()),
        (culprit_core::key!("view.mysql_events.field.ends"), event.ends_without_offset.as_deref()),
        (culprit_core::key!("view.mysql_events.field.last_started"), event.last_started_without_offset.as_deref()),
        (culprit_core::key!("view.mysql_events.field.created"), event.created_without_offset.as_deref()),
        (culprit_core::key!("view.mysql_events.field.altered"), event.altered_without_offset.as_deref()),
    ] {
        metrics.push(Metric::said_text(key, value.unwrap_or(&unknown)));
    }
    Ok(JobEntry {
        source: row.source.clone(),
        observation_key: row.key.clone(),
        job_id: None,
        kind: JobEntryKind::State,
        title: t!("view.mysql_events.event.title"),
        explanation: match event.status {
            Some(MysqlEventStatus::Enabled) => t!("view.mysql_events.event.enabled"),
            Some(MysqlEventStatus::Disabled) => t!("view.mysql_events.event.disabled"),
            Some(MysqlEventStatus::ReplicaSideDisabled | MysqlEventStatus::SlavesideDisabled) => {
                t!("view.mysql_events.event.replica_disabled")
            }
            _ => t!("view.mysql_events.event.unknown"),
        },
        metrics,
        investigation: t!("view.mysql_events.investigate"),
        verify: t!("view.mysql_events.verify"),
    })
}
fn invalid() -> Error {
    Error::invalid(t!("view.jobs.invalid"))
}
#[cfg(test)]
mod tests;
