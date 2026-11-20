//! Bounded output uses shared exact display values and keeps every finite qualification.
use super::statement::preview;
use super::{SourceQualification, Summary, render_summary};
use culprit_core::{PerformanceCounterReport, format, t};

pub(crate) fn performance_counters(report: &PerformanceCounterReport) -> String {
    let mut text = render_summary(
        Summary {
            title: &report.title,
            summary: &report.summary,
            source: SourceQualification::Single { state: report.source_state, reason: report.source_reason.as_deref() },
            limitations: &report.limitations,
        },
        |_| {},
    );
    if let (Some(database), Some(id)) = (&report.database, &report.native_database_id) {
        text.push('\n');
        text.push_str(&t!("view.performance_counters.database", database = preview(database), id = id));
    }
    text.push('\n');
    text.push_str(&t!(
        "view.performance_counters.counts",
        native_sets = count(report.native_observations),
        sets = format::digits(report.observed_observations.to_string()),
        native = count(report.native_counter_rows),
        observed = format::digits(report.observed_counter_rows.to_string()),
        presented = format::digits(report.presented_counter_rows.to_string())
    ));
    text.push('\n');
    text.push_str(&t!(
        "view.performance_counters.cli.printed",
        printed = format::digits(report.entries.len().min(super::super::BRIEF_FINDING_LIMIT).to_string())
    ));
    if let Some(context) = &report.context {
        let (from, until) = context.bounds();
        text.push('\n');
        text.push_str(&t!("view.wait_counters.bounds", from = from, until = until));
    }
    for entry in report.entries.iter().take(super::super::BRIEF_FINDING_LIMIT) {
        text.push_str("\n\n");
        text.push_str(&entry.family_label);
        text.push('\n');
        text.push_str(&entry.explanation);
        text.push('\n');
        text.push_str(&t!(
            "view.counter_ratios.cli.metric",
            label = &entry.metric.label,
            value = entry.metric.text.as_deref().unwrap_or_default()
        ));
        text.push('\n');
        text.push_str(&t!(
            "view.performance_counters.cli.native",
            object = preview(&entry.native_object),
            name = preview(&entry.native_name),
            instance =
                entry.native_instance.as_deref().map(preview).unwrap_or_else(|| t!("view.counter_ratios.uninstanced")),
            native_type = entry.native_type.clone().unwrap_or_else(|| t!("view.performance_counters.unknown")),
            raw = &entry.raw_value,
            at = entry.observed_at.clone().unwrap_or_else(|| t!("view.performance_counters.unknown"))
        ));
    }
    if !report.missing_fields.is_empty() {
        text.push('\n');
        text.push_str(&t!(
            "view.performance_counters.missing",
            count = format::digits(report.missing_fields.len().to_string())
        ));
    }
    if let Some(ratios) = &report.ratios {
        text.push_str("\n\n");
        text.push_str(&super::counter_ratios(ratios));
    }
    for limitation in report.limitations.iter().skip(1) {
        text.push('\n');
        text.push_str(limitation);
    }
    text
}
fn count(count: Option<u32>) -> String {
    count.map(|value| format::digits(value.to_string())).unwrap_or_else(|| t!("view.performance_counters.unknown"))
}

#[cfg(test)]
mod tests;
