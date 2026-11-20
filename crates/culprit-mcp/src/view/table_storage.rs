//! Project only the selected table's persisted native configuration, never a memory or data-loss verdict.
use super::*;
use culprit_core::{Engine, Snapshot, SourceState, TableStat, source_id, t};

pub(super) fn configuration(snapshot: &Snapshot, table: &TableStat) -> Option<Value> {
    if snapshot.server.engine != Engine::Mssql {
        return None;
    }
    let configuration = table.mssql_storage_configuration.as_ref().filter(|value| value.is_valid())?;
    let source = snapshot.sources.iter().find(|source| source.source == source_id::TABLES);
    if source.is_some_and(|source| !matches!(source.state, SourceState::Collected | SourceState::Partial)) {
        return None;
    }
    let source_qualification = match source.map(|source| source.state) {
        Some(SourceState::Partial) => Some(t!("view.table_storage.partial")),
        None => Some(t!("view.table_storage.unknown_source")),
        _ => None,
    };
    Some(json!({
        "source_state":source.map(|source|source.state),
        "memory_optimized":configuration.memory_optimized,
        "durability":configuration.durability,
        "durability_desc":configuration.durability_desc,
        "qualification":t!("view.table_storage.qualification"),
        "source_qualification":source_qualification,
    }))
}
