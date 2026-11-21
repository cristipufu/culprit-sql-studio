//! Human output renders the canonical fields with explicit presentation bounds and existing terminal escaping.
use super::{SourceQualification, Summary, render_summary, statement::preview};
use culprit_core::{IndexConfigurationReport, format, t};
const ENTRY_LIMIT: usize = 3;
const PARTITION_LIMIT: usize = 8;
pub(crate) fn index_configuration(report: &IndexConfigurationReport) -> String {
    let mut output = render_summary(
        Summary {
            title: &report.title,
            summary: &report.summary,
            source: SourceQualification::Single { state: report.source_state, reason: report.source_reason.as_deref() },
            limitations: &report.limitations,
        },
        |_| {},
    );
    output.push('\n');
    output.push_str(&report.qualification);
    if let Some(context) = &report.context {
        output.push('\n');
        output.push_str(&t!(
            "view.wait_counters.bounds",
            from = &context.observed_from,
            until = &context.observed_until
        ));
    }
    for entry in report.entries.iter().take(ENTRY_LIMIT) {
        append_entry(&mut output, entry);
    }
    if report.entries.len() > ENTRY_LIMIT {
        output.push('\n');
        output.push_str(&t!(
            "view.index_configuration.presentation_limited",
            shown = ENTRY_LIMIT,
            total = report.entries.len()
        ));
    }
    for limitation in report.limitations.iter().skip(1) {
        output.push('\n');
        output.push_str(limitation);
    }
    output
}
pub(super) fn append_entry(output: &mut String, entry: &culprit_core::IndexConfigurationEntry) {
    output.push_str("\n\n");
    output.push_str(&t!(
        "view.index_configuration.entry",
        schema = preview(&entry.parent_schema),
        table = preview(&entry.parent_name),
        index = entry.index_name.as_deref().map(preview).unwrap_or_else(|| t!("view.index_configuration.heap"))
    ));
    for field in &entry.fields {
        output.push('\n');
        output.push_str(&t!("view.counter_ratios.cli.metric", label = &field.label, value = preview(&field.text)));
    }
    for partition in entry.partitions.iter().take(PARTITION_LIMIT) {
        output.push('\n');
        output.push_str(&t!(
            "view.index_configuration.partition",
            number = format::digits(partition.partition_number.to_string()),
            id = format::digits(partition.partition_id.clone()),
            hobt = format::digits(partition.hobt_id.clone())
        ));
        for field in &partition.fields {
            output.push('\n');
            output.push_str(&t!("view.counter_ratios.cli.metric", label = &field.label, value = preview(&field.text)));
        }
    }
    if entry.retained_partitions as usize > PARTITION_LIMIT {
        output.push('\n');
        output.push_str(&t!(
            "view.index_configuration.partition_presentation_limited",
            shown = entry.partitions.len().min(PARTITION_LIMIT),
            total = entry.retained_partitions
        ));
    }
    for limitation in &entry.limitations {
        output.push('\n');
        output.push_str(limitation);
    }
}
pub(crate) fn compact_index_configuration(report: &IndexConfigurationReport) -> serde_json::Value {
    serde_json::json!({
        "dataset":report.dataset,"title":report.title,"summary":report.summary,"qualification":report.qualification,
        "historical_context":report.context.is_some(),"read_bounds":report.context.as_ref().map(|context|serde_json::json!({"from":context.observed_from,"until":context.observed_until})),
        "source_state":report.source_state,"source_reason":report.source_reason,"read_complete":report.read_complete,"native_rows":report.native_rows,
        "observed_rows":report.observed_rows,"returned_rows":report.entries.len().min(ENTRY_LIMIT),"entries_limited":report.entries_limited||report.entries.len()>ENTRY_LIMIT,
        "entries":report.entries.iter().take(ENTRY_LIMIT).map(|entry|serde_json::json!({
            "parent_kind":entry.parent_kind,"parent_schema":preview(&entry.parent_schema),"parent_name":preview(&entry.parent_name),"index_name":entry.index_name.as_deref().map(preview),
            "parent_object":entry.parent_object,"index_id":entry.index_id,"fields":entry.fields,
            "parent_modified_without_offset":entry.parent_modified_without_offset,"partition_identity_availability":entry.partition_identity_availability,
            "retained_partitions":entry.retained_partitions,"partitions_complete":entry.partitions_complete,
            "returned_partitions":entry.partitions.len().min(PARTITION_LIMIT),"partitions_limited":entry.partitions_limited||entry.partitions.len()>PARTITION_LIMIT,
            "partitions":entry.partitions.iter().take(PARTITION_LIMIT).collect::<Vec<_>>(),"limitations":entry.limitations
        })).collect::<Vec<_>>(),"limitations":report.limitations
    })
}
#[cfg(test)]
mod tests;
