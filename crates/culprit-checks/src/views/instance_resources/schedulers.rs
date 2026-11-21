//! Per-scheduler context is independent of the aggregate resource point and identifies no database cause.
use culprit_core::{
    Engine, ResetMarker, Result, SchedulerEntry, SchedulerField, SchedulerFieldAvailability, SchedulerReport, Snapshot,
    SourceState, format, t, telemetry::*,
};
use std::collections::BTreeSet;

pub const MAX_PRESENTED_SCHEDULERS: usize = 256;

pub(super) fn explain_source(
    source: &CollectionDescription,
    coverage: &StateSourceCoverage,
    observations: &[&OperationalObservation],
    read_complete: bool,
) -> Result<SchedulerReport> {
    let context = NativeReadContext::from_snapshot(source)?;
    if !valid_owner(&context)
        || source.observation.snapshot().is_none_or(|header| {
            header.dataset != SnapshotDataset::Operational || !header.sources.iter().any(|value| value == coverage)
        })
        || coverage.source != culprit_core::source_id::SCHEDULERS
        || coverage.records as usize > MAX_MSSQL_SCHEDULERS
        || observations.len() > coverage.records as usize
        || read_complete && observations.len() != coverage.records as usize
        || !observations.is_empty() && !matches!(coverage.state, SourceState::Collected | SourceState::Partial)
    {
        return Err(invalid());
    }
    let mut samples = Vec::with_capacity(observations.len());
    for row in observations {
        if !row.is_valid() || row.source != coverage.source {
            return Err(invalid());
        }
        let OperationalFact::CounterSet(sample) = &row.fact else { return Err(invalid()) };
        samples.push(sample.as_ref());
    }
    let mut report = render(&samples, coverage.state)?;
    report.context = Some(ObservationReadContext::Native { context: Box::new(context.clone()) });
    report.native_source = Some(coverage.clone());
    report.native_rows = Some(coverage.records);
    report.read_complete = Some(read_complete);
    report.source_reason = coverage.error_code.map(|_| t!("view.schedulers.source_error"));
    if !read_complete && matches!(coverage.state, SourceState::Collected | SourceState::Empty | SourceState::Partial) {
        report.source_state = SourceState::Partial;
        report.limitations.push(t!("view.schedulers.limit.read"));
    }
    if !context.has_qualified_local_owner() {
        report.limitations.push(t!("view.schedulers.limit.owner"));
    }
    qualify(&mut report);
    Ok(report)
}

pub(super) fn explain_snapshot(snapshot: &Snapshot) -> Result<Option<SchedulerReport>> {
    let samples = snapshot
        .pressure
        .as_ref()
        .and_then(|pressure| pressure.resource_counters.as_deref())
        .unwrap_or_default()
        .iter()
        .filter(|sample| scheduler_candidate(sample))
        .collect::<Vec<_>>();
    let source = snapshot.sources.iter().find(|source| source.source == culprit_core::source_id::SCHEDULERS);
    if samples.is_empty() && source.is_none() {
        return Ok(None);
    }
    if snapshot.server.engine != Engine::Mssql {
        return Err(invalid());
    }
    if !super::super::source_coverage::consistent_source(
        source,
        samples.iter().map(|_| culprit_core::source_id::SCHEDULERS),
    ) {
        return Err(invalid());
    }
    let mut report = render(&samples, source.map_or(SourceState::Unavailable, |source| source.state))?;
    report.source_reason = source.and_then(|source| source.reason.clone());
    report.limitations.push(t!("view.schedulers.limit.original"));
    if samples.is_empty() && matches!(report.source_state, SourceState::Collected | SourceState::Partial) {
        report.source_state = SourceState::Partial;
        report.limitations.push(t!("view.schedulers.limit.read"));
    }
    qualify(&mut report);
    Ok(Some(report))
}

fn valid_owner(context: &NativeReadContext) -> bool {
    context.is_valid()
        && matches!(&context.target, CollectionTarget::Shared(resource)
        if resource.engine == Engine::Mssql && resource.kind == SharedResourceKind::Instance)
}
fn render(samples: &[&NativeCounterSet], state: SourceState) -> Result<SchedulerReport> {
    if samples.len() > MAX_MSSQL_SCHEDULERS {
        return Err(invalid());
    }
    let mut keys = BTreeSet::new();
    // Validate ALL retained rows before presentation; an invalid/duplicate tail cannot hide beyond the cap.
    let points = samples.iter().map(|sample| MssqlSchedulerPoint::from_sample(sample)).collect::<Result<Vec<_>>>()?;
    for point in &points {
        if !keys.insert(point.sample().key()?) {
            return Err(invalid());
        }
    }
    let entries = points.iter().take(MAX_PRESENTED_SCHEDULERS).map(entry).collect::<Result<Vec<_>>>()?;
    let mut report = empty(state);
    report.observed_rows = samples.len() as u32;
    report.presented_rows = entries.len() as u32;
    report.entries_limited = entries.len() < samples.len();
    if !entries.is_empty() {
        report.summary = t!("view.schedulers.summary.recorded");
    }
    report.entries = entries;
    Ok(report)
}
fn empty(state: SourceState) -> SchedulerReport {
    SchedulerReport {
        scope: culprit_core::MeasurementScope::Server,
        population: String::new(),
        context: None,
        native_source: None,
        source_state: state,
        source_reason: None,
        read_complete: None,
        native_rows: None,
        observed_rows: 0,
        presented_rows: 0,
        entries_limited: false,
        title: t!("view.schedulers.title"),
        summary: t!("view.schedulers.summary.unavailable"),
        limitations: vec![
            t!("view.schedulers.limit.scope"),
            t!("view.schedulers.limit.clock"),
            t!("view.schedulers.limit.counters"),
        ],
        entries: vec![],
    }
}
fn entry(point: &MssqlSchedulerPoint<'_>) -> Result<SchedulerEntry> {
    let mut fields = Vec::with_capacity(MSSQL_SCHEDULER_FIELDS.len() - 1);
    for field in MSSQL_SCHEDULER_FIELDS.iter().filter(|field| !matches!(field.kind, MssqlSchedulerFieldKind::Identity))
    {
        let (value, availability) = match point.field(field)? {
            MssqlSchedulerValue::Observed(value) => (Some(value.into()), SchedulerFieldAvailability::Observed),
            MssqlSchedulerValue::NativeNull => (None, SchedulerFieldAvailability::NativeNull),
            MssqlSchedulerValue::ColumnUnavailable => (None, SchedulerFieldAvailability::ColumnUnavailable),
        };
        let (unit, nature, reset) = match field.kind {
            MssqlSchedulerFieldKind::Counter { unit, nature } => (Some(unit), Some(nature), Some(ResetMarker::Unknown)),
            _ => (None, None, None),
        };
        fields.push(SchedulerField {
            native_name: field.name.into(),
            label: label(field.name)?,
            value,
            availability,
            explanation: match (unit, nature) {
                (Some(CounterUnit::Unknown), _) => Some(t!("view.schedulers.counter.unknown")),
                (_, Some(CounterNature::Cumulative)) => Some(t!("view.schedulers.counter.cumulative")),
                _ => None,
            },
            unit,
            nature,
            reset,
        });
    }
    Ok(SchedulerEntry {
        observation_key: point.sample().key()?,
        scheduler_id: point.scheduler_id().into(),
        title: t!("view.schedulers.entry", id = format::digits(point.scheduler_id().into())),
        fields,
    })
}
fn label(name: &str) -> Result<String> {
    Ok(match name {
        "parent_node_id" => t!("view.schedulers.field.parent_node_id"),
        "cpu_id" => t!("view.schedulers.field.cpu_id"),
        "status" => t!("view.schedulers.field.status"),
        "is_online" => t!("view.schedulers.field.is_online"),
        "is_idle" => t!("view.schedulers.field.is_idle"),
        "failed_to_create_worker" => t!("view.schedulers.field.failed_to_create_worker"),
        "current_tasks_count" => t!("view.schedulers.field.current_tasks_count"),
        "runnable_tasks_count" => t!("view.schedulers.field.runnable_tasks_count"),
        "current_workers_count" => t!("view.schedulers.field.current_workers_count"),
        "active_workers_count" => t!("view.schedulers.field.active_workers_count"),
        "work_queue_count" => t!("view.schedulers.field.work_queue_count"),
        "pending_disk_io_count" => t!("view.schedulers.field.pending_disk_io_count"),
        "preemptive_switches_count" => t!("view.schedulers.field.preemptive_switches_count"),
        "context_switches_count" => t!("view.schedulers.field.context_switches_count"),
        "idle_switches_count" => t!("view.schedulers.field.idle_switches_count"),
        "load_factor" => t!("view.schedulers.field.load_factor"),
        "yield_count" => t!("view.schedulers.field.yield_count"),
        "last_timer_activity" => t!("view.schedulers.field.last_timer_activity"),
        "total_cpu_usage_ms" => t!("view.schedulers.field.total_cpu_usage_ms"),
        "total_scheduler_delay_ms" => t!("view.schedulers.field.total_scheduler_delay_ms"),
        "ideal_workers_limit" => t!("view.schedulers.field.ideal_workers_limit"),
        _ => return Err(invalid()),
    })
}
fn qualify(report: &mut SchedulerReport) {
    report.population = t!(
        "view.schedulers.population",
        native = report
            .native_rows
            .map(|rows| format::digits(rows.to_string()))
            .unwrap_or_else(|| t!("view.database_properties.unknown")),
        observed = format::digits(report.observed_rows.to_string()),
        presented = format::digits(report.presented_rows.to_string())
    );
    if report.source_state == SourceState::Partial {
        report.limitations.push(t!("view.schedulers.limit.partial"));
    }
    if report.entries_limited {
        report.limitations.push(t!("view.schedulers.limit.presentation"));
    }
}
pub(super) fn invalid_report() -> SchedulerReport {
    let mut report = empty(SourceState::Unavailable);
    report.limitations.push(t!("view.schedulers.invalid"));
    qualify(&mut report);
    report
}
fn invalid() -> culprit_core::Error {
    CollectionStop::InvalidPage.error()
}

#[cfg(test)]
mod tests;
