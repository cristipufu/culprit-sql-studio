//! One interpretation of retained native shared-instance resource facts; no native access or pressure grading.
use culprit_core::{
    InstanceResourceField, InstanceResourceGroup, InstanceResourceReport, Result, Snapshot, SourceState, t,
    telemetry::*,
};

pub fn explain_snapshot(snapshot: &Snapshot) -> Result<Option<InstanceResourceReport>> {
    if let Some(read) = snapshot.pressure.as_ref().and_then(|pressure| pressure.instance_resources.as_deref()) {
        return explain(read).map(Some);
    }
    let Some(source) = snapshot.sources.iter().find(|source| source.source == INSTANCE_RESOURCES_SOURCE) else {
        return Ok(None);
    };
    let mut report = unavailable();
    report.source_state = match source.state {
        SourceState::Collected | SourceState::Empty | SourceState::Partial => SourceState::Partial,
        state => state,
    };
    Ok(Some(report))
}

pub fn explain_captured(read: &CapturedOperationalRead) -> Result<InstanceResourceReport> {
    if !read.is_valid() {
        return Err(CollectionStop::InvalidPage.error());
    }
    let point = InstanceResourceRead::from_source(&read.description, &read.observations, read.read_complete)?;
    explain(&point)
}

pub fn explain(read: &InstanceResourceRead) -> Result<InstanceResourceReport> {
    if !read.is_valid() {
        return Err(CollectionStop::InvalidPage.error());
    }
    let mut report = unavailable();
    report.context = Some(read.context.clone());
    report.native_source = Some(read.source.clone());
    report.source_state =
        if !read.read_complete && matches!(read.source.state, SourceState::Collected | SourceState::Partial) {
            SourceState::Partial
        } else {
            read.source.state
        };
    report.read_complete = Some(read.read_complete);
    if !read.read_complete {
        report.limitations.push(t!("view.instance_resources.limit.read"));
    }
    let Some(point) = read.observation.as_deref() else {
        return Ok(report);
    };
    report.observed_at = Some(point.observed_at.clone());
    report.native_populations_complete = Some(point.has_complete_population());
    report.summary = if report.source_state == SourceState::Partial {
        t!("view.instance_resources.summary.partial")
    } else {
        t!("view.instance_resources.summary.recorded")
    };
    report.groups = groups(point);
    if !point.has_complete_population() {
        report.limitations.push(t!("view.instance_resources.limit.population"));
    }
    Ok(report)
}

fn unavailable() -> InstanceResourceReport {
    InstanceResourceReport {
        context: None,
        native_source: None,
        source_state: SourceState::Unavailable,
        read_complete: None,
        observed_at: None,
        native_populations_complete: None,
        title: t!("view.instance_resources.title"),
        summary: t!("view.instance_resources.summary.unavailable"),
        limitations: vec![
            t!("view.instance_resources.limit.scope"),
            t!("view.instance_resources.limit.point"),
            t!("view.instance_resources.limit.cause"),
            t!("view.instance_resources.limit.wait"),
        ],
        groups: vec![],
    }
}

fn groups(point: &MssqlInstanceResourceObservation) -> Vec<InstanceResourceGroup> {
    let schedulers = &point.schedulers;
    let memory = &point.memory;
    let pending = &point.pending_io;
    let waits = &point.io_waits;
    vec![
        group(
            "schedulers",
            t!("view.instance_resources.group.schedulers"),
            vec![
                count(
                    "scheduler_population_rows",
                    t!("view.instance_resources.field.scheduler_population_rows"),
                    schedulers.population_rows,
                ),
                count("scheduler_rows", t!("view.instance_resources.field.scheduler_rows"), schedulers.observed_rows),
                count(
                    "scheduler_unknowns",
                    t!("view.instance_resources.field.scheduler_unknowns"),
                    schedulers.unknown_values,
                ),
                value("runnable_tasks", t!("view.instance_resources.field.runnable_tasks"), &schedulers.runnable_tasks),
                value("worker_queue", t!("view.instance_resources.field.worker_queue"), &schedulers.worker_queue),
                value(
                    "failed_worker_creations",
                    t!("view.instance_resources.field.failed_worker_creations"),
                    &schedulers.failed_worker_creations,
                ),
            ],
        ),
        group(
            "process-memory",
            t!("view.instance_resources.group.process_memory"),
            vec![
                count("process_rows", t!("view.instance_resources.field.process_rows"), memory.process_rows),
                count(
                    "process_unknowns",
                    t!("view.instance_resources.field.process_unknowns"),
                    memory.unknown_process_flags,
                ),
                flag(
                    "process_physical_low",
                    t!("view.instance_resources.field.process_physical_low"),
                    memory.process_physical_low,
                ),
                flag(
                    "process_virtual_low",
                    t!("view.instance_resources.field.process_virtual_low"),
                    memory.process_virtual_low,
                ),
                value(
                    "physical_used_kib",
                    t!("view.instance_resources.field.physical_used_kib"),
                    &memory.physical_used_kib,
                ),
                value(
                    "virtual_committed_kib",
                    t!("view.instance_resources.field.virtual_committed_kib"),
                    &memory.virtual_committed_kib,
                ),
                value(
                    "virtual_available_kib",
                    t!("view.instance_resources.field.virtual_available_kib"),
                    &memory.virtual_available_kib,
                ),
            ],
        ),
        group(
            "system-memory",
            t!("view.instance_resources.group.system_memory"),
            vec![
                count("system_rows", t!("view.instance_resources.field.system_rows"), memory.system_rows),
                count(
                    "system_unknowns",
                    t!("view.instance_resources.field.system_unknowns"),
                    memory.unknown_system_flags,
                ),
                flag("system_low", t!("view.instance_resources.field.system_low"), memory.system_low),
                value(
                    "system_total_kib",
                    t!("view.instance_resources.field.system_total_kib"),
                    &memory.system_total_kib,
                ),
                value(
                    "system_available_kib",
                    t!("view.instance_resources.field.system_available_kib"),
                    &memory.system_available_kib,
                ),
            ],
        ),
        group(
            "grant-waiters",
            t!("view.instance_resources.group.grant_waiters"),
            vec![
                count("grant_rows", t!("view.instance_resources.field.grant_rows"), point.grant_rows),
                count("grant_waiters", t!("view.instance_resources.field.grant_waiters"), point.grant_waiters),
            ],
        ),
        group(
            "pending-io",
            t!("view.instance_resources.group.pending_io"),
            vec![
                count("pending_io_rows", t!("view.instance_resources.field.pending_io_rows"), pending.observed_rows),
                count(
                    "pending_io_unknowns",
                    t!("view.instance_resources.field.pending_io_unknowns"),
                    pending.unknown_flags,
                ),
                value("os_pending", t!("view.instance_resources.field.os_pending"), &pending.os_pending),
                value(
                    "completion_pending",
                    t!("view.instance_resources.field.completion_pending"),
                    &pending.completion_pending,
                ),
            ],
        ),
        group(
            "io-waits",
            t!("view.instance_resources.group.io_waits"),
            vec![
                count(
                    "io_wait_population_rows",
                    t!("view.instance_resources.field.io_wait_population_rows"),
                    waits.population_rows,
                ),
                count(
                    "io_wait_unknown_types",
                    t!("view.instance_resources.field.io_wait_unknown_types"),
                    waits.unknown_types,
                ),
                count("io_wait_rows", t!("view.instance_resources.field.io_wait_rows"), waits.observed_rows),
                count(
                    "io_wait_unknowns",
                    t!("view.instance_resources.field.io_wait_unknowns"),
                    waits.unknown_durations,
                ),
                value("longest_wait_ms", t!("view.instance_resources.field.longest_wait_ms"), &waits.longest_wait_ms),
            ],
        ),
    ]
}
fn group(key: &str, title: String, fields: Vec<InstanceResourceField>) -> InstanceResourceGroup {
    InstanceResourceGroup { key: key.into(), title, fields }
}
fn count(key: &str, label: String, value: u32) -> InstanceResourceField {
    InstanceResourceField { key: key.into(), label, value: Some(value.to_string()) }
}
fn flag(key: &str, label: String, value: Option<bool>) -> InstanceResourceField {
    InstanceResourceField { key: key.into(), label, value: value.map(|value| u8::from(value).to_string()) }
}
fn value(key: &str, label: String, value: &Option<String>) -> InstanceResourceField {
    InstanceResourceField { key: key.into(), label, value: value.clone() }
}

#[cfg(test)]
mod tests;
