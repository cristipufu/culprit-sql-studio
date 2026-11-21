//! Current table-only candidates cannot borrow an indexed view or an unknown catalog subject.
use super::*;

pub(crate) fn permits_table_index_advice(snapshot: &Snapshot, schema: &str, name: &str) -> bool {
    if snapshot.server.engine != Engine::Mssql {
        return true;
    }
    if snapshot.sources.iter().any(|source| {
        source.source == culprit_core::source_id::TABLES
            && matches!(source.state, culprit_core::SourceState::Unavailable | culprit_core::SourceState::NotApplicable)
    }) {
        return false;
    }
    let matches = |parent_schema: &str, parent_name: &str| {
        parent_schema.eq_ignore_ascii_case(schema) && parent_name.eq_ignore_ascii_case(name)
    };
    if let Some(catalog) = &snapshot.operations.indexed_views {
        if !catalog.is_valid()
            || matches!(
                catalog.source.state,
                culprit_core::SourceState::Unavailable | culprit_core::SourceState::NotApplicable
            )
            || catalog.indexes.iter().any(|index| matches(&index.parent_schema, &index.parent_name))
        {
            return false;
        }
    }
    // This is current advice admission only. A matching label does not bind a historical plan incarnation.
    snapshot.tables.iter().filter(|table| matches(&table.schema, &table.name)).count() == 1
}
