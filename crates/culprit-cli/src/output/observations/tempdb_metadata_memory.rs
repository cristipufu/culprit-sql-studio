//! The archive text renderer uses the same canonical native points and words as the SDK and local MCP action.
use super::{SourceQualification, Summary, render_summary};
use culprit_core::{TempdbMetadataMemoryEntry, TempdbMetadataMemoryReport, format, t};

pub(crate) fn tempdb_metadata_memory(report: &TempdbMetadataMemoryReport) -> String {
    let mut rendered = render_summary(
        Summary {
            title: &report.title,
            summary: &report.summary,
            source: SourceQualification::Single { state: Some(report.source_state), reason: None },
            limitations: &report.limitations,
        },
        |text| {
            append_profile(text, report);
            for entry in report.entries.iter().take(super::super::BRIEF_FINDING_LIMIT) {
                append_consumer(text, entry);
            }
            if report.entries.len() > super::super::BRIEF_FINDING_LIMIT {
                text.push('\n');
                text.push_str(&t!("view.tempdb_metadata_memory.limit.presentation"));
            }
        },
    );
    for limitation in report.limitations.iter().skip(1) {
        rendered.push('\n');
        rendered.push_str(&detail(limitation));
    }
    rendered
}

fn append_profile(text: &mut String, report: &TempdbMetadataMemoryReport) {
    let (from, until) = report.context.bounds();
    text.push('\n');
    text.push_str(&t!("view.wait_counters.bounds", from = from, until = until));
    text.push('\n');
    text.push_str(&t!(
        "view.wait_counters.cli.counts",
        native = format::digits(report.native_rows.to_string()),
        observed = format::digits(report.observed_rows.to_string()),
        presented = format::digits(report.entries.len().to_string()),
        printed = format::digits(report.entries.len().min(super::super::BRIEF_FINDING_LIMIT).to_string())
    ));
    field(
        text,
        &t!("view.tempdb_metadata_memory.field.enabled"),
        report.metadata_enabled.map(|enabled| format::digits(u8::from(enabled).to_string())),
    );
    field(text, &t!("view.tempdb_metadata_memory.field.feature_observed_at"), report.feature_observed_at.clone());
    field(text, &t!("view.tempdb_metadata_memory.field.created"), report.tempdb_created_without_offset.clone());
}

fn append_consumer(text: &mut String, entry: &TempdbMetadataMemoryEntry) {
    text.push_str("\n\n");
    text.push_str(&detail(&entry.native_type_name));
    if let Some(description) = &entry.description {
        text.push_str(" · ");
        text.push_str(&detail(description));
    }
    field(
        text,
        &t!("view.tempdb_metadata_memory.field.consumer"),
        Some(format::digits(entry.native_consumer_id.clone())),
    );
    field(text, &t!("view.storage.field.observed_at"), Some(entry.observed_at.clone()));
    field(text, &t!("view.tempdb_metadata_memory.field.reserved"), entry.allocated_bytes.clone().map(format::digits));
    field(text, &t!("view.tempdb_metadata_memory.field.used"), entry.used_bytes.clone().map(format::digits));
    for context in [&entry.explanation, &entry.investigation, &entry.verify] {
        text.push('\n');
        text.push_str(&detail(context));
    }
}

fn field(text: &mut String, label: &str, value: Option<String>) {
    text.push('\n');
    text.push_str(&t!(
        "view.counter_ratios.cli.metric",
        label = label,
        value = detail(&value.unwrap_or_else(|| t!("view.wait_counters.unknown")))
    ));
}
fn detail(value: &str) -> String {
    super::super::brief_detail(value, super::OBSERVATION_DETAIL_CHARS)
}

#[cfg(test)]
mod tests;
