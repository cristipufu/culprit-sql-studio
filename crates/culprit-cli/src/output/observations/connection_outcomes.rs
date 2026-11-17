use super::{SourceQualification, Summary, render_summary};
use culprit_core::{ConnectionOutcomeReport, format, t};

const ENTRY_LIMIT: usize = 8;

pub(crate) fn connection_outcomes(report: &ConnectionOutcomeReport) -> String {
    let (from, until) = report.context.bounds();
    let mut text = render_summary(
        Summary {
            title: &report.title,
            summary: &report.summary,
            source: SourceQualification::Multiple(&report.sources),
            limitations: &report.limitations,
        },
        |text| {
            text.push_str(&t!("view.connection_outcomes.cli.bounds", from = from, until = until));
            text.push('\n');
            text.push_str(&t!(
                "view.connection_outcomes.cli.counts",
                native = format::digits(report.native_rows.to_string()),
                observed = format::digits(report.observed_rows.to_string()),
                mapped = format::digits(report.mapped_rows.to_string()),
                shown = format::digits(report.entries.len().min(ENTRY_LIMIT).to_string())
            ));
            for entry in report.entries.iter().take(ENTRY_LIMIT) {
                text.push_str("\n\n");
                text.push_str(&entry.title);
                text.push('\n');
                text.push_str(&entry.explanation);
                text.push('\n');
                text.push_str(&t!(
                    "view.connection_outcomes.cli.reference",
                    source = &entry.reference.source,
                    key = &entry.reference.observation_key
                ));
                for metric in &entry.metrics {
                    let value = metric.display_value.clone().unwrap_or_else(|| {
                        t!(
                            "view.connection_outcomes.cli.missing",
                            state = culprit_core::wire_name(&metric.availability)
                        )
                    });
                    text.push('\n');
                    text.push_str(&t!("view.connection_outcomes.cli.metric", label = &metric.label, value = value));
                    text.push('\n');
                    text.push_str(&metric.explanation);
                }
                text.push('\n');
                text.push_str(&entry.investigation);
                text.push('\n');
                text.push_str(&entry.verify);
            }
            if !report.entries_limited && report.entries.len() > ENTRY_LIMIT {
                text.push('\n');
                text.push_str(&t!("view.connection_outcomes.limit.presentation"));
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
