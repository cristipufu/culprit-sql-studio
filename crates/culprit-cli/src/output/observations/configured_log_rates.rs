use super::{SourceQualification, Summary, render_summary};
use culprit_core::{ConfiguredLogRateReport, t};

pub(crate) fn configured_log_rates(report: &ConfiguredLogRateReport) -> String {
    let mut text = render_summary(
        Summary {
            title: &report.title,
            summary: &report.summary,
            source: SourceQualification::Single {
                state: Some(report.source_state),
                reason: report.source_reason.as_deref(),
            },
            limitations: &report.limitations,
        },
        |text| {
            text.push('\n');
            text.push_str(&t!(
                "view.connection_outcomes.cli.bounds",
                from = &report.context.observed_from,
                until = &report.context.observed_until
            ));
            if let Some(at) = &report.observed_at {
                text.push('\n');
                text.push_str(&t!("view.counter_ratios.cli.clock", at = at));
            }
            for entry in &report.entries {
                text.push_str("\n\n");
                text.push_str(&entry.title);
                text.push('\n');
                text.push_str(&entry.explanation);
                text.push('\n');
                text.push_str(&t!(
                    "view.counter_ratios.cli.metric",
                    label = &entry.metric.label,
                    value = entry.metric.text.as_deref().unwrap_or("")
                ));
            }
        },
    );
    for limitation in report.limitations.iter().skip(1) {
        text.push('\n');
        text.push_str(limitation);
    }
    text
}

#[cfg(test)]
mod tests;
