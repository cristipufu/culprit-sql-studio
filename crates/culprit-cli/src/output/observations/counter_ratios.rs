//! This text projection shares the native ratio decision and exact display values with the app and SDK.
use super::statement::preview;
use super::{SourceQualification, Summary, render_summary};
use culprit_core::{CounterRatioReport, t};

pub(crate) fn counter_ratios(report: &CounterRatioReport) -> String {
    let mut rendered = render_summary(
        Summary {
            title: &report.title,
            summary: &report.summary,
            source: SourceQualification::Single { state: report.source_state, reason: report.source_reason.as_deref() },
            limitations: &report.limitations,
        },
        |text| {
            for entry in report.entries.iter().take(super::super::BRIEF_FINDING_LIMIT) {
                text.push_str("\n\n");
                text.push_str(&entry.title);
                text.push('\n');
                text.push_str(&entry.explanation);
                if let Some(object) = &entry.native_object {
                    text.push('\n');
                    text.push_str(&t!(
                        "view.counter_ratios.cli.population",
                        object = preview(object),
                        instance = entry
                            .native_instance
                            .as_deref()
                            .map(preview)
                            .unwrap_or_else(|| t!("view.counter_ratios.uninstanced"))
                    ));
                }
                for metric in &entry.metrics {
                    text.push('\n');
                    text.push_str(&t!(
                        "view.counter_ratios.cli.metric",
                        label = &metric.label,
                        value = metric.text.as_deref().unwrap_or("")
                    ));
                }
                if let Some(at) = &entry.observed_at {
                    text.push('\n');
                    text.push_str(&t!("view.counter_ratios.cli.clock", at = at));
                }
            }
            if report.entries.len() > super::super::BRIEF_FINDING_LIMIT {
                text.push('\n');
                text.push_str(&t!("view.counter_ratios.limit.presentation"));
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
