//! Exact field labels and source qualifications come from the same report as the SDK and UI.
use super::{SourceQualification, Summary, render_summary};
use culprit_core::{MemoryClerkReport, format, t};

pub(crate) fn memory_clerks(report: &MemoryClerkReport) -> String {
    let mut rendered = render_summary(
        Summary {
            title: &report.title,
            summary: &report.summary,
            source: SourceQualification::Single { state: report.source_state, reason: report.source_reason.as_deref() },
            limitations: &report.limitations,
        },
        |text| {
            text.push('\n');
            text.push_str(&t!(
                "view.wait_counters.cli.counts",
                native = report
                    .native_rows
                    .map(|value| format::digits(value.to_string()))
                    .unwrap_or_else(|| t!("collected.memory_clerks.unknown")),
                observed = format::digits(report.observed_rows.to_string()),
                presented = format::digits(report.presented_rows.to_string()),
                printed = format::digits(report.entries.len().min(super::super::BRIEF_FINDING_LIMIT).to_string())
            ));
            if let Some(context) = &report.context {
                let (from, until) = context.bounds();
                text.push('\n');
                text.push_str(&t!("view.wait_counters.bounds", from = from, until = until));
            }
            for entry in report.entries.iter().take(super::super::BRIEF_FINDING_LIMIT) {
                text.push_str("\n\n");
                text.push_str(&entry.clerk_type);
                text.push('\n');
                text.push_str(&entry.explanation);
                for (label, value) in [
                    (t!("collected.memory_clerks.native.name"), entry.name.clone()),
                    (t!("collected.memory_clerks.native.node"), entry.memory_node_id.clone()),
                ] {
                    text.push('\n');
                    text.push_str(&t!("view.counter_ratios.cli.metric", label = label, value = value));
                }
                for field in &entry.fields {
                    let value = field
                        .value
                        .clone()
                        .map(format::digits)
                        .unwrap_or_else(|| t!("collected.memory_clerks.unknown"));
                    text.push('\n');
                    text.push_str(&t!("view.counter_ratios.cli.metric", label = &field.label, value = value));
                }
            }
            if report.entries.len() > super::super::BRIEF_FINDING_LIMIT {
                text.push('\n');
                text.push_str(&t!("collected.memory_clerks.limit.presentation"));
            }
        },
    );
    for limitation in report.limitations.iter().skip(1) {
        rendered.push('\n');
        rendered.push_str(limitation);
    }
    rendered
}

#[cfg(test)]
mod tests;
