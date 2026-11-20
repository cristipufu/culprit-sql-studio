use super::{OBSERVATION_DETAIL_CHARS, SourceQualification, Summary, render_summary, statement};
use culprit_core::{TableFullTextReport, t};

pub(crate) fn table_full_text(report: &TableFullTextReport) -> String {
    render_summary(
        Summary {
            title: &report.title,
            summary: &report.summary,
            source: SourceQualification::Single { state: report.source_state, reason: report.source_reason.as_deref() },
            limitations: &report.limitations,
        },
        |text| {
            if let (Some(from), Some(until)) = (&report.observed_from, &report.observed_until) {
                text.push('\n');
                text.push_str(&t!("view.wait_counters.bounds", from = from, until = until));
            }
            let entries = report
                .entries
                .iter()
                .take(super::super::BRIEF_FINDING_LIMIT)
                .map(|entry| {
                    let mut details = format!(
                        "{}.{}: {}",
                        statement::preview(&entry.schema),
                        statement::preview(&entry.table),
                        entry.summary
                    );
                    for metric in &entry.metrics {
                        if let Some(value) = &metric.text {
                            details.push_str(" · ");
                            details.push_str(&t!(
                                "view.counter_ratios.cli.metric",
                                label = &metric.label,
                                value = value
                            ));
                        }
                    }
                    details.push(' ');
                    details.push_str(&entry.qualification);
                    details.push(' ');
                    details.push_str(&entry.verify);
                    details
                })
                .collect::<Vec<_>>();
            super::super::append_brief_details(text, entries.iter().map(String::as_str), OBSERVATION_DETAIL_CHARS);
        },
    )
}

#[cfg(test)]
mod tests;
