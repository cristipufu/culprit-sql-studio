//! CDC text consumes the canonical explanation, with the existing observation/item and text bounds.
use super::{SourceQualification, Summary, append_remaining_limitations, native_work_summary, statement};
use culprit_core::{CdcReport, t};

const DISPLAY_FIELDS: usize = 5;
pub(crate) fn cdc(report: &CdcReport) -> String {
    let mut text = native_work_summary(
        Summary {
            title: &report.title,
            summary: &report.summary,
            source: SourceQualification::Single { state: Some(report.source_state), reason: None },
            limitations: &report.limitations,
        },
        report.entries.iter().map(|entry| {
            let mut details = vec![entry.explanation.clone()];
            details.extend(entry.fields.iter().take(DISPLAY_FIELDS).map(|field| {
                t!(
                    "view.counter_ratios.cli.metric",
                    label = field.label.clone(),
                    value = field
                        .value
                        .as_deref()
                        .map(statement::preview)
                        .unwrap_or_else(|| t!("view.wait_counters.unknown"))
                )
            }));
            details.extend([entry.investigation.clone(), entry.verify.clone()]);
            (entry.title.as_str(), details)
        }),
    );
    append_remaining_limitations(&mut text, &report.limitations);
    text
}
#[cfg(test)]
mod tests;
