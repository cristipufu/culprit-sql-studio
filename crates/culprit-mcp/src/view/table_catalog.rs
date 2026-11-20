//! Project only the selected table's original native catalog, with its source qualification.
use super::*;
use culprit_core::{Engine, Snapshot, SourceState, TableStat, source_id, t};

struct CatalogSource {
    state: Option<SourceState>,
    qualification: Option<String>,
}
fn catalog_source(snapshot: &Snapshot) -> Option<CatalogSource> {
    if snapshot.server.engine != Engine::Mssql {
        return None;
    }
    let source = snapshot.sources.iter().find(|source| source.source == source_id::TABLES);
    if source.is_some_and(|source| !matches!(source.state, SourceState::Collected | SourceState::Partial)) {
        return None;
    }
    let state = source.map(|source| source.state);
    let qualification = match state {
        Some(SourceState::Partial) => Some(t!("view.table_catalog.partial")),
        None => Some(t!("view.table_catalog.unknown_source")),
        _ => None,
    };
    Some(CatalogSource { state, qualification })
}

pub(super) fn configuration(snapshot: &Snapshot, table: &TableStat) -> Option<Value> {
    let configuration = table.mssql_storage_configuration.as_ref().filter(|value| value.is_valid())?;
    let source = catalog_source(snapshot)?;
    Some(json!({
        "source_state":source.state,
        "memory_optimized":configuration.memory_optimized,
        "durability":configuration.durability,
        "durability_desc":configuration.durability_desc,
        "qualification":t!("view.table_storage.qualification"),
        "source_qualification":source.qualification,
    }))
}

pub(super) fn change_data_capture(snapshot: &Snapshot, table: &TableStat) -> Option<Value> {
    let source = catalog_source(snapshot)?;
    Some(json!({
        "tracked_by_cdc":table.mssql_tracked_by_cdc,
        "source_state":source.state,
        "source_qualification":source.qualification,
        "qualification":t!("view.table_cdc.qualification"),
    }))
}
