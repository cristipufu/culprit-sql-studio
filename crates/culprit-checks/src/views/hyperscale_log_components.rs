//! A pure explanation of native per-component reports, independent of waits, throughput and statement causes.
use culprit_core::{
    HyperscaleLogComponentEntry, HyperscaleLogComponentReport, Metric, Result, Snapshot, SourceState, format, t,
    telemetry::*,
};
pub const MAX_HYPERSCALE_LOG_COMPONENT_ENTRIES: usize = 256;

pub fn explain_snapshot(snapshot: &Snapshot) -> Result<Option<HyperscaleLogComponentReport>> {
    let Some(read) = snapshot.operations.hyperscale_log_components.as_deref() else { return Ok(None) };
    if !read.is_valid() || !read.description.target.matches_native_database(snapshot) {
        return Err(invalid());
    }
    explain_captured(read).map(Some)
}
pub fn explain_captured(read: &CapturedOperationalRead) -> Result<HyperscaleLogComponentReport> {
    if !read.is_valid() {
        return Err(invalid());
    }
    let input =
        HyperscaleLogComponentRead::from_source(&read.description, read.observations.clone(), read.read_complete)?;
    explain(&input)
}
pub fn explain(read: &HyperscaleLogComponentRead) -> Result<HyperscaleLogComponentReport> {
    if !read.is_valid() {
        return Err(invalid());
    }
    let context = read.context()?;
    let source = read.coverage()?;
    let missing = read.observations.len() < source.records as usize || !read.read_complete;
    let readable = matches!(source.state, SourceState::Collected | SourceState::Empty | SourceState::Partial);
    let partial = source.state == SourceState::Partial || readable && missing;
    let mut limitations = vec![
        t!("view.hyperscale_log_components.limit.values"),
        t!("view.hyperscale_log_components.limit.clock"),
        t!("view.hyperscale_log_components.limit.cause"),
        t!("view.hyperscale_log_components.limit.visibility"),
        t!("view.hyperscale_log_components.verify"),
    ];
    let execution = read.description.observation.execution_context();
    if execution.map_or(!context.has_qualified_local_owner(), |context| !context.has_qualified_local_owner()) {
        limitations.push(t!("view.hyperscale_log_components.limit.identity"));
    }
    if execution.and_then(|context| context.database_writable).or(context.database_writable).is_none() {
        limitations.push(t!("view.hyperscale_log_components.limit.role"));
    }
    if partial {
        limitations.push(t!("view.hyperscale_log_components.limit.partial"));
    }
    let limited = read.observations.len() > MAX_HYPERSCALE_LOG_COMPONENT_ENTRIES;
    if limited {
        limitations.push(t!("view.hyperscale_log_components.limit.presentation"));
    }
    let entries = read
        .observations
        .iter()
        .take(MAX_HYPERSCALE_LOG_COMPONENT_ENTRIES)
        .map(|row| {
            let OperationalFact::HyperscaleLogComponent(point) = &row.fact else { return Err(invalid()) };
            Ok(HyperscaleLogComponentEntry {
                observation_key: row.key.clone(),
                component_id: point.component_id.clone(),
                replica_id: point.replica_id.clone(),
                file_id: point.file_id.clone(),
                current_max_log_rate_bps: point.current_max_log_rate_bps.clone(),
                catchup_rate_bps: point.catchup_rate_bps.clone(),
                catchup_distance_bytes: point.catchup_distance_bytes.clone(),
                role: point.role,
                role_desc: point.role_desc.clone(),
                last_reported_time_without_offset: point.last_reported_time_without_offset.clone(),
                title: role(point.role),
                explanation: t!("view.hyperscale_log_components.entry"),
                metrics: vec![
                    Metric::text(
                        t!("view.hyperscale_log_components.field.limit"),
                        rate(&point.current_max_log_rate_bps),
                    ),
                    Metric::text(t!("view.hyperscale_log_components.field.rate"), rate(&point.catchup_rate_bps)),
                    Metric::text(
                        t!("view.hyperscale_log_components.field.distance"),
                        t!(
                            "view.hyperscale_log_components.bytes",
                            value = format::digits(point.catchup_distance_bytes.clone())
                        ),
                    ),
                ],
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(HyperscaleLogComponentReport {
        context: context.clone(),
        native_source: source.clone(),
        source_state: if partial { SourceState::Partial } else { source.state },
        source_reason: (source.state == SourceState::Unavailable)
            .then(|| t!("view.hyperscale_log_components.unavailable")),
        read_complete: read.read_complete,
        native_rows: source.records,
        observed_rows: read.observations.len() as u32,
        presented_rows: entries.len() as u32,
        entries_limited: limited,
        title: t!("view.hyperscale_log_components.title"),
        summary: if !readable {
            t!("view.hyperscale_log_components.summary.unavailable")
        } else if partial {
            t!("view.hyperscale_log_components.summary.partial")
        } else if entries.is_empty() {
            t!("view.hyperscale_log_components.summary.empty")
        } else {
            t!(
                "view.hyperscale_log_components.summary.observed",
                count = format::digits(read.observations.len().to_string())
            )
        },
        limitations,
        entries,
    })
}
fn rate(value: &str) -> String {
    t!("view.hyperscale_log_components.rate", value = format::digits(value.to_owned()))
}
fn role(role: u8) -> String {
    match role {
        0 => t!("view.hyperscale_log_components.role.unknown"),
        1 => t!("view.hyperscale_log_components.role.storage"),
        2 => t!("view.hyperscale_log_components.role.primary"),
        4 => t!("view.hyperscale_log_components.role.replica"),
        5 => t!("view.hyperscale_log_components.role.local_destage"),
        6 => t!("view.hyperscale_log_components.role.destage"),
        10 => t!("view.hyperscale_log_components.role.geo_replica"),
        12 => t!("view.hyperscale_log_components.role.checkpoint"),
        14 => t!("view.hyperscale_log_components.role.migration"),
        _ => t!("view.hyperscale_log_components.role.unrecognized"),
    }
}
fn invalid() -> culprit_core::Error {
    culprit_core::Error::invalid(t!("view.hyperscale_log_components.invalid"))
}
#[cfg(test)]
mod tests;
