//! One bounded canonical projection serves the current report and authenticated local-original route.
use culprit_core::{IndexConfigurationReport, Report};
use serde_json::{Value, json};
const ENTRY_LIMIT: usize = 3;
const PARTITION_LIMIT: usize = 8;
pub(super) fn attach(value: &mut Value, report: &Report) {
    if !report.index_configuration.is_empty() {
        value["index_configuration"] = json!(report.index_configuration.iter().map(self::value).collect::<Vec<_>>());
    }
}
pub(crate) fn value(report: &IndexConfigurationReport) -> Value {
    json!({
        "dataset":report.dataset,"historical_context":report.context.is_some(),
        "read_bounds":report.context.as_ref().map(|context|json!({"from":context.observed_from,"until":context.observed_until})),
        "title":report.title,"summary":report.summary,"qualification":report.qualification,
        "source_state":report.source_state,"source_reason":report.source_reason,"read_complete":report.read_complete,
        "native_rows":report.native_rows,"observed_rows":report.observed_rows,"presented_rows":report.presented_rows,
        "returned_rows":report.entries.len().min(ENTRY_LIMIT),"entries_limited":report.entries_limited||report.entries.len()>ENTRY_LIMIT,
        "entries":report.entries.iter().take(ENTRY_LIMIT).map(entry_value).collect::<Vec<_>>(),"limitations":report.limitations,
    })
}
#[cfg(test)]
mod tests;

pub(crate) fn entry_value(entry: &culprit_core::IndexConfigurationEntry) -> Value {
    json!({
        "parent_schema":crate::statement_context::bounded_text(&entry.parent_schema,256),
        "parent_name":crate::statement_context::bounded_text(&entry.parent_name,256),
        "index_name":entry.index_name.as_deref().map(|name|crate::statement_context::bounded_text(name,256)),
        "parent_kind":entry.parent_kind,"parent_object":entry.parent_object,"index_id":entry.index_id,"type_code":entry.type_code,
        "parent_modified_without_offset":entry.parent_modified_without_offset,"fields":entry.fields,
        "partition_identity_availability":entry.partition_identity_availability,"partitions_complete":entry.partitions_complete,
        "retained_partitions":entry.retained_partitions,"returned_partitions":entry.partitions.len().min(PARTITION_LIMIT),
        "partitions_limited":entry.partitions_limited||entry.partitions.len()>PARTITION_LIMIT,
        "partitions":entry.partitions.iter().take(PARTITION_LIMIT).collect::<Vec<_>>(),"limitations":entry.limitations,
    })
}
