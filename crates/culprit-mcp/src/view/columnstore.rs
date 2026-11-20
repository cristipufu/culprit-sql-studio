//! Compact current catalog facts inside the selected original index; no rowgroup or health interpretation.
use super::*;
const MAX_COLUMNS: usize = 16;

pub(super) fn configuration(snapshot: &culprit_core::Snapshot, index: &culprit_core::IndexStat) -> Option<Value> {
    if snapshot.server.engine != culprit_core::Engine::Mssql {
        return None;
    }
    let configuration = index.mssql_columnstore.as_ref().filter(|value| value.is_for_index(index))?;
    let source = snapshot.sources.iter().find(|source| source.source == culprit_core::source_id::INDEXES);
    if source.is_some_and(|source| source.state == culprit_core::SourceState::Unavailable) {
        return None;
    }
    Some(json!({
        "source_state":source.map(|source|source.state),
        "native_table":configuration.table,
        "native_index_id":configuration.index_id,
        "table_modified_without_offset":configuration.table_modified_without_offset,
        "compression_delay_availability":configuration.compression_delay_availability,
        "compression_delay_minutes":configuration.compression_delay_minutes,
        "column_store_order_availability":configuration.column_store_order_availability,
        "data_clustering_availability":configuration.data_clustering_availability,
        "columns_total":configuration.columns.len(),
        "columns_shown":configuration.columns.len().min(MAX_COLUMNS),
        "columns":configuration.columns.iter().take(MAX_COLUMNS).map(|column|json!({
            "index_column_id":column.index_column_id,"column_id":column.column_id,"name":clip(&column.name,TEXT_SHORT),
            "column_store_order_ordinal":column.column_store_order_ordinal,"data_clustering_ordinal":column.data_clustering_ordinal,
        })).collect::<Vec<_>>(),
    }))
}

#[cfg(test)]
mod tests;
