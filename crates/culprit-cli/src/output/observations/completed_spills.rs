//! Bounded text renders the same exact metrics and receipt-backed statement preview as JSON.
use super::*;
use culprit_core::CompletedSpillReport;

pub(crate) fn completed_spills(report: &CompletedSpillReport) -> String {
    let mut text = native_work_summary(
        Summary {
            title: &report.title,
            summary: &report.summary,
            source: SourceQualification::Multiple(&report.sources),
            limitations: &report.limitations,
        },
        report.entries.iter().map(|entry| {
            let mut details = vec![entry.explanation.clone()];
            details.extend(native_metrics(&entry.metrics));
            if let Some(sql) = &entry.statement_preview {
                details.push(t!("evidence.cli.completed_spills.statement", sql = sql));
            }
            if entry.statement_unavailable.is_some() {
                details.push(t!("evidence.cli.completed_spills.payload_unavailable"));
            }
            details.extend([entry.investigation.clone(), entry.verify.clone()]);
            (entry.title.as_str(), details)
        }),
    );
    // These finite source qualifications apply to every row; a finding-count limit must not hide a role or read gap.
    for limitation in report.limitations.iter().skip(1) {
        super::super::append_brief_details(&mut text, std::iter::once(limitation.as_str()), OBSERVATION_DETAIL_CHARS);
    }
    text.push('\n');
    text.push_str(&t!(
        "evidence.cli.completed_spills.bounds",
        from = &report.context.observed_from,
        until = &report.context.observed_until
    ));
    text.push('\n');
    text.push_str(&t!(
        "evidence.cli.completed_spills.counts",
        native = format::digits(report.native_rows.to_string()),
        observed = format::digits(report.observed_rows.to_string())
    ));
    if let Some(context) = &report.definition_context {
        text.push('\n');
        text.push_str(&t!(
            "evidence.cli.completed_spills.definition_bounds",
            from = &context.observed_from,
            until = &context.observed_until
        ));
        append_native_sources(&mut text, &report.definition_sources);
    }
    text
}

#[cfg(test)]
mod tests;
