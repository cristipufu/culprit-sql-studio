//! Original/current views share one bounded canonical projection; no selected table is inferred from names.
use culprit_core::{IndexedViewsReport, Report};
use serde_json::{Value, json};
const ENTRY_LIMIT: usize = 3;
const COLUMN_LIMIT: usize = 8;
pub(super) fn attach(output: &mut Value, report: &Report) {
    if !report.indexed_views.is_empty() {
        output["indexed_views"] = json!(report.indexed_views.iter().map(value).collect::<Vec<_>>());
    }
}
pub(crate) fn value(report: &IndexedViewsReport) -> Value {
    json!({"title":report.title,"summary":report.summary,"qualification":report.qualification,
        "historical_context":report.context.is_some(),"read_bounds":report.context.as_ref().map(|context|json!({"from":context.observed_from,"until":context.observed_until})),
        "source":report.source,"read_complete":report.read_complete,"native_index_rows":report.native_index_rows,
        "observed_index_rows":report.observed_index_rows,"observed_view_indexes":report.observed_view_indexes,"returned_entries":report.entries.len().min(ENTRY_LIMIT),"entries_limited":report.entries_limited||report.entries.len()>ENTRY_LIMIT,
        "entries":report.entries.iter().take(ENTRY_LIMIT).map(|entry|json!({
            "parent_object":entry.parent_object,"parent_schema":crate::statement_context::bounded_text(&entry.parent_schema,256),"parent_name":crate::statement_context::bounded_text(&entry.parent_name,256),"parent_modified_without_offset":entry.parent_modified_without_offset,
            "index_id":entry.index_id,"index_name":crate::statement_context::bounded_text(&entry.index_name,256),"type_code":entry.type_code,"native_type":crate::statement_context::bounded_text(&entry.native_type,256),"unique":entry.unique,"disabled":entry.disabled,"reserved_bytes":entry.reserved_bytes,
            "native_columns":entry.native_columns,"returned_columns":entry.columns.len().min(COLUMN_LIMIT),"columns_limited":entry.columns_limited||entry.columns.len()>COLUMN_LIMIT,
            "columns":entry.columns.iter().take(COLUMN_LIMIT).map(|column|json!({"column_id":column.column_id,"index_column_id":column.index_column_id,"name":crate::statement_context::bounded_text(&column.name,256),"key_ordinal":column.key_ordinal,"descending":column.descending,"included":column.included})).collect::<Vec<_>>(),
            "usage":entry.usage,"configuration":entry.configuration.as_deref().map(super::index_configuration::entry_value),
        })).collect::<Vec<_>>(),"limitations":report.limitations})
}
#[cfg(test)]
mod tests;
