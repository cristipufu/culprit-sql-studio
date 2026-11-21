//! Bounded human/compact output consumes only the canonical indexed-view report.
use super::{SourceQualification, Summary, render_summary, statement::preview};
use culprit_core::{IndexedViewsReport, format, key, t, telemetry::MssqlIndexUsage};
const ENTRY_LIMIT: usize = 3;
const COLUMN_LIMIT: usize = 8;
pub(crate) fn indexed_views(report: &IndexedViewsReport) -> String {
    let mut output = render_summary(
        Summary {
            title: &report.title,
            summary: &report.summary,
            source: SourceQualification::Single {
                state: Some(report.source.state),
                reason: report.source.reason.as_deref(),
            },
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
        output.push_str("\n\n");
        output.push_str(&t!(
            "view.indexed_views.entry",
            schema = preview(&entry.parent_schema),
            parent = preview(&entry.parent_name),
            index = preview(&entry.index_name),
            native_type = preview(&entry.native_type)
        ));
        output.push('\n');
        output.push_str(&t!(
            "view.indexed_views.index_state",
            unique = if entry.unique { "true" } else { "false" },
            disabled = if entry.disabled { "true" } else { "false" }
        ));
        if let Some(bytes) = &entry.reserved_bytes {
            output.push('\n');
            output.push_str(&t!("view.indexed_views.reserved", bytes = format::digits(bytes.clone())));
        }
        match &entry.usage {
            MssqlIndexUsage::Observed(usage) => {
                output.push('\n');
                output.push_str(&t!(
                    "view.indexed_views.usage",
                    state = if usage.row_present { "present" } else { "absent" }
                ));
                if usage.row_present {
                    for (label, value) in [
                        (key!("view.indexed_views.user_seeks"), &usage.user_seeks),
                        (key!("view.indexed_views.user_scans"), &usage.user_scans),
                        (key!("view.indexed_views.user_lookups"), &usage.user_lookups),
                        (key!("view.indexed_views.user_updates"), &usage.user_updates),
                    ] {
                        output.push('\n');
                        output.push_str(&t!(
                            "view.counter_ratios.cli.metric",
                            label = culprit_core::i18n::t(label, &[]),
                            value = value
                                .as_ref()
                                .map(|value| format::digits(value.clone()))
                                .unwrap_or_else(|| t!("ui.memory_failures.not_reported"))
                        ));
                    }
                }
            }
            MssqlIndexUsage::NotCollected => {
                output.push('\n');
                output.push_str(&t!("view.indexed_views.usage", state = "not-collected"));
            }
            MssqlIndexUsage::NotApplicable => {
                output.push('\n');
                output.push_str(&t!("view.indexed_views.usage", state = "not-applicable"));
            }
        }
        if let Some(configuration) = &entry.configuration {
            super::index_configuration::append_entry(&mut output, configuration);
        }
    }
    if report.entries.len() > ENTRY_LIMIT {
        output.push('\n');
        output.push_str(&t!(
            "view.indexed_views.presentation_limited",
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
pub(crate) fn compact_indexed_views(report: &IndexedViewsReport) -> serde_json::Value {
    serde_json::json!({"title":report.title,"summary":report.summary,"qualification":report.qualification,
        "historical_context":report.context.is_some(),"read_bounds":report.context.as_ref().map(|context|serde_json::json!({"from":context.observed_from,"until":context.observed_until})),
        "source":report.source,"read_complete":report.read_complete,"native_index_rows":report.native_index_rows,
        "observed_index_rows":report.observed_index_rows,"observed_view_indexes":report.observed_view_indexes,"returned_entries":report.entries.len().min(ENTRY_LIMIT),"entries_limited":report.entries_limited||report.entries.len()>ENTRY_LIMIT,
        "entries":report.entries.iter().take(ENTRY_LIMIT).map(|entry|serde_json::json!({
            "parent_object":entry.parent_object,"parent_schema":preview(&entry.parent_schema),"parent_name":preview(&entry.parent_name),"parent_modified_without_offset":entry.parent_modified_without_offset,
            "index_id":entry.index_id,"index_name":preview(&entry.index_name),"type_code":entry.type_code,"native_type":preview(&entry.native_type),"unique":entry.unique,"disabled":entry.disabled,"reserved_bytes":entry.reserved_bytes,
            "native_columns":entry.native_columns,"columns_limited":entry.columns_limited||entry.columns.len()>COLUMN_LIMIT,"columns":entry.columns.iter().take(COLUMN_LIMIT).map(|column|serde_json::json!({"column_id":column.column_id,"index_column_id":column.index_column_id,"name":preview(&column.name),"key_ordinal":column.key_ordinal,"descending":column.descending,"included":column.included})).collect::<Vec<_>>(),"usage":entry.usage,
        })).collect::<Vec<_>>(),"limitations":report.limitations})
}
#[cfg(test)]
mod tests;
