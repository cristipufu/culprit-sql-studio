use super::{SourceQualification, Summary, render_summary};
use culprit_core::{StorageReport, format, t};
pub(crate) fn storage(report: &StorageReport) -> String {
    render_summary(
        Summary {
            title: &report.title,
            summary: &report.summary,
            source: SourceQualification::Multiple(&report.sources),
            limitations: &report.limitations,
        },
        |text| {
            let (from, until) = report.context.bounds();
            text.push('\n');
            text.push_str(&t!("view.wait_counters.bounds", from = from, until = until));
            text.push('\n');
            text.push_str(&t!(
                "view.wait_counters.cli.counts",
                native = format::digits(report.native_rows.to_string()),
                observed = format::digits(report.observed_rows.to_string()),
                presented = format::digits(report.presented_rows.to_string()),
                printed = format::digits(report.entries.len().min(super::super::BRIEF_FINDING_LIMIT).to_string())
            ));
            if let Some(summary) = &report.file_summary {
                text.push_str("\n\n");
                text.push_str(&summary.title);
                text.push('\n');
                text.push_str(&summary.summary);
                for field in &summary.fields {
                    text.push('\n');
                    let value = field
                        .value
                        .as_deref()
                        .map(|value| super::statement::preview_bounded(value, 1024))
                        .unwrap_or_else(|| t!("ui.memory_failures.not_reported"));
                    text.push_str(&t!("view.counter_ratios.cli.metric", label = &field.label, value = value));
                }
                for qualification in &summary.limitations {
                    text.push('\n');
                    text.push_str(qualification);
                }
            }
            for entry in report.entries.iter().take(super::super::BRIEF_FINDING_LIMIT) {
                text.push_str("\n\n");
                text.push_str(&entry.title);
                text.push('\n');
                text.push_str(&entry.explanation);
                for field in &entry.fields {
                    text.push('\n');
                    let value = field.value.clone().unwrap_or_else(|| t!("ui.memory_failures.not_reported"));
                    text.push_str(&t!("view.counter_ratios.cli.metric", label = &field.label, value = value));
                }
            }
        },
    )
}
#[cfg(test)]
mod tests;
