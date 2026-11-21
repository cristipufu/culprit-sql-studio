//! Current source positions, not physical-topology joins, CPU percentages or memory pressure causes.
use super::*;
use culprit_core::{
    NumaNodeEntry, NumaNodeField, NumaNodeFieldAvailability as Availability, NumaNodePopulationReport, NumaNodeReport,
    ResetMarker,
};
use std::collections::BTreeSet;
const DISPLAY_NODES: usize = 256;
pub(super) fn explain_source(
    source: &CollectionDescription,
    rows: &[OperationalObservation],
    complete: bool,
) -> Result<InstanceResourceReport> {
    if !numa_node_header_is_valid(source) || rows.iter().any(|row| !row.is_valid()) {
        return Err(invalid());
    }
    let header = source.observation.snapshot().ok_or_else(invalid)?;
    if rows.len() > header.records as usize || complete && rows.len() != header.records as usize {
        return Err(invalid());
    }
    let mut populations = Vec::new();
    for kind in MssqlNumaNodeKind::ALL {
        let coverage = header.sources.iter().find(|coverage| coverage.source == kind.source()).ok_or_else(invalid)?;
        let native_rows = rows.iter().filter(|row| row.source == kind.source()).collect::<Vec<_>>();
        if native_rows.len() > coverage.records as usize
            || complete && native_rows.len() != coverage.records as usize
            || !native_rows.is_empty() && !matches!(coverage.state, SourceState::Collected | SourceState::Partial)
        {
            return Err(invalid());
        }
        let mut unique = BTreeSet::new();
        let mut entries = Vec::new();
        for row in &native_rows {
            let OperationalFact::CounterSet(sample) = &row.fact else { return Err(invalid()) };
            let point = MssqlNumaNodePoint::from_sample(sample)?;
            if point.kind() != kind || !unique.insert(point.node_id()) {
                return Err(invalid());
            }
            if entries.len() < DISPLAY_NODES {
                entries.push(entry(&point)?)
            }
        }
        let state = if !complete && matches!(coverage.state, SourceState::Collected | SourceState::Empty) {
            SourceState::Partial
        } else {
            coverage.state
        };
        populations.push(NumaNodePopulationReport {
            kind,
            native_source: coverage.clone(),
            source_state: state,
            native_rows: coverage.records,
            observed_rows: native_rows.len() as u32,
            entries_limited: native_rows.len() > entries.len(),
            title: title(kind),
            summary: t!(
                "view.numa_nodes.population",
                native = culprit_core::format::digits(coverage.records.to_string()),
                observed = culprit_core::format::digits(native_rows.len().to_string())
            ),
            entries,
        });
    }
    if rows.iter().any(|row| !MssqlNumaNodeKind::ALL.into_iter().any(|kind| row.source == kind.source())) {
        return Err(invalid());
    }
    let mut report = super::unavailable();
    report.title = t!("view.numa_nodes.title");
    report.summary = t!("view.numa_nodes.summary");
    report.limitations = limits();
    report.read_complete = Some(complete);
    report.source_state = if complete
        && populations
            .iter()
            .all(|population| matches!(population.source_state, SourceState::Collected | SourceState::Empty))
    {
        SourceState::Collected
    } else if populations.iter().any(|population| {
        matches!(population.source_state, SourceState::Collected | SourceState::Empty | SourceState::Partial)
    }) {
        SourceState::Partial
    } else {
        SourceState::Unavailable
    };
    report.numa_nodes = Some(Box::new(NumaNodeReport {
        scope: culprit_core::MeasurementScope::Server,
        context: Some(ObservationReadContext::Native { context: Box::new(NativeReadContext::from_snapshot(source)?) }),
        read_complete: complete,
        title: report.title.clone(),
        summary: report.summary.clone(),
        limitations: report.limitations.clone(),
        populations,
    }));
    Ok(report)
}
fn entry(point: &MssqlNumaNodePoint<'_>) -> Result<NumaNodeEntry> {
    let mut fields = Vec::new();
    for field in point.kind().fields().iter().filter(|field| !matches!(field.kind, MssqlNativeFieldKind::Identity)) {
        let (value, availability) = match point.field(field)? {
            MssqlNativeFieldValue::Observed(value) => (Some(value.into()), Availability::Observed),
            MssqlNativeFieldValue::NativeNull => (None, Availability::NativeNull),
            MssqlNativeFieldValue::ColumnUnavailable => (None, Availability::ColumnUnavailable),
            MssqlNativeFieldValue::Unqualified { .. } => return Err(invalid()),
        };
        let (unit, nature, reset) = match field.kind {
            MssqlNativeFieldKind::Counter { unit, nature } => (Some(unit), Some(nature), Some(ResetMarker::Unknown)),
            _ => (None, None, None),
        };
        fields.push(NumaNodeField {
            native_name: field.name.into(),
            label: label(field.name)?,
            value,
            availability,
            unit,
            nature,
            reset,
        });
    }
    Ok(NumaNodeEntry {
        observation_key: point.sample().key()?,
        node_id: point.node_id().into(),
        title: t!("view.numa_nodes.entry", id = point.node_id()),
        fields,
    })
}
fn title(kind: MssqlNumaNodeKind) -> String {
    match kind {
        MssqlNumaNodeKind::Cpu => t!("view.numa_nodes.cpu"),
        MssqlNumaNodeKind::Memory => t!("view.numa_nodes.memory"),
    }
}
fn limits() -> Vec<String> {
    vec![
        t!("view.numa_nodes.limit.scope"),
        t!("view.numa_nodes.limit.topology"),
        t!("view.numa_nodes.limit.memory"),
        t!("view.numa_nodes.limit.original"),
    ]
}
pub fn unavailable(reason: &culprit_core::i18n::Message) -> InstanceResourceReport {
    let mut report = super::unavailable();
    report.title = t!("view.numa_nodes.title");
    report.summary = reason.render();
    report.limitations = limits();
    report.numa_nodes = Some(Box::new(NumaNodeReport {
        scope: culprit_core::MeasurementScope::Server,
        context: None,
        read_complete: false,
        title: report.title.clone(),
        summary: report.summary.clone(),
        limitations: report.limitations.clone(),
        populations: vec![],
    }));
    report
}
fn label(name: &str) -> Result<String> {
    Ok(match name {
        "node_state_desc" => t!("view.numa_nodes.field.node_state_desc"),
        "memory_node_id" => t!("view.numa_nodes.field.memory_node_id"),
        "cpu_affinity_mask" => t!("view.numa_nodes.field.cpu_affinity_mask"),
        "online_scheduler_count" => t!("view.numa_nodes.field.online_scheduler_count"),
        "idle_scheduler_count" => t!("view.numa_nodes.field.idle_scheduler_count"),
        "active_worker_count" => t!("view.numa_nodes.field.active_worker_count"),
        "avg_load_balance" => t!("view.numa_nodes.field.avg_load_balance"),
        "timer_task_affinity_mask" => t!("view.numa_nodes.field.timer_task_affinity_mask"),
        "permanent_task_affinity_mask" => t!("view.numa_nodes.field.permanent_task_affinity_mask"),
        "resource_monitor_state" => t!("view.numa_nodes.field.resource_monitor_state"),
        "online_scheduler_mask" => t!("view.numa_nodes.field.online_scheduler_mask"),
        "processor_group" => t!("view.numa_nodes.field.processor_group"),
        "cpu_count" => t!("view.numa_nodes.field.cpu_count"),
        "virtual_address_space_reserved_kb" => t!("view.numa_nodes.field.virtual_address_space_reserved_kb"),
        "virtual_address_space_committed_kb" => t!("view.numa_nodes.field.virtual_address_space_committed_kb"),
        "locked_page_allocations_kb" => t!("view.numa_nodes.field.locked_page_allocations_kb"),
        "pages_kb" => t!("view.numa_nodes.field.pages_kb"),
        "shared_memory_reserved_kb" => t!("view.numa_nodes.field.shared_memory_reserved_kb"),
        "shared_memory_committed_kb" => t!("view.numa_nodes.field.shared_memory_committed_kb"),
        "foreign_committed_kb" => t!("view.numa_nodes.field.foreign_committed_kb"),
        "target_kb" => t!("view.numa_nodes.field.target_kb"),
        _ => return Err(invalid()),
    })
}
fn invalid() -> culprit_core::Error {
    CollectionStop::InvalidPage.error()
}
#[cfg(test)]
mod tests;
