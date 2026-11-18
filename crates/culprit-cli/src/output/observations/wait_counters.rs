//! Bounded output reuses the exact shared explanation; no native wait owner or reset is inferred here.
use super::{SourceQualification, Summary, render_summary};
use culprit_core::{WaitCounterReport, format, t};

pub(crate) fn wait_counters(report: &WaitCounterReport) -> String {
    render_summary(
        Summary {
            title: &report.title,
            summary: &report.summary,
            source: SourceQualification::Multiple(&report.sources),
            limitations: &report.limitations,
        },
        |text| {
            text.push('\n');
            text.push_str(&t!(
                "view.wait_counters.cli.counts",
                native = format::digits(report.native_rows.to_string()),
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
                text.push_str(&entry.title);
                text.push('\n');
                text.push_str(&entry.explanation);
                for metric in &entry.metrics {
                    text.push('\n');
                    text.push_str(&t!(
                        "view.counter_ratios.cli.metric",
                        label = &metric.label,
                        value = metric.text.as_deref().unwrap_or("")
                    ));
                }
            }
            if report.entries.len() > super::super::BRIEF_FINDING_LIMIT {
                text.push('\n');
                text.push_str(&t!("view.wait_counters.limit.presentation"));
            }
            for limitation in report.limitations.iter().skip(1) {
                text.push('\n');
                text.push_str(limitation);
            }
        },
    )
}

#[cfg(test)]
mod tests;
