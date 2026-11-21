//! Concise native fragment output reuses the canonical report and the shared bounded text formatter.
use super::{
    SourceQualification, Summary, append_remaining_limitations, native_metrics, native_work_summary, statement,
};
use culprit_core::{query_wait_report::QueryWaitFragmentReport, t};
pub(crate) fn query_wait_fragments(report: &QueryWaitFragmentReport) -> String {
    let unknown = t!("view.query_wait_fragments.unknown");
    let entries = report
        .entries
        .iter()
        .map(|entry| {
            let label = t!(
                "view.query_wait_fragments.entry",
                category = statement::preview(entry.category_description.as_deref().unwrap_or(&entry.category)),
                outcome = statement::preview(entry.outcome_description.as_deref().unwrap_or(&unknown))
            );
            (label, native_metrics(&entry.metrics).collect::<Vec<_>>())
        })
        .collect::<Vec<_>>();
    let mut text = native_work_summary(
        Summary {
            title: &report.title,
            summary: &report.summary,
            source: SourceQualification::Single {
                state: Some(report.source_state),
                reason: report.source_reason.as_deref(),
            },
            limitations: &report.limitations,
        },
        entries.iter().map(|(label, metrics)| (label.as_str(), metrics.clone())),
    );
    append_remaining_limitations(&mut text, &report.limitations);
    text
}
pub(crate) fn compact_query_wait_fragments(report: &QueryWaitFragmentReport) -> serde_json::Value {
    let entries = report.entries.iter().take(5).map(|entry|serde_json::json!({"category":entry.category,"category_description":entry.category_description,"outcome_description":entry.outcome_description,"unit":entry.unit,"metrics":entry.metrics})).collect::<Vec<_>>();
    serde_json::json!({"title":report.title,"summary":report.summary,"source_state":report.source_state,"source_reason":report.source_reason,"limitations":report.limitations,"selected_rows":report.selected_rows,"observed_rows":report.observed_rows,"capture":report.capture,"interval_coverage":report.interval_coverage,"references_complete":report.references_complete,"read_complete":report.read_complete,"entries_available":report.observed_rows,"entries_presented":entries.len(),"entries_limited":report.presentation_limited || report.entries.len()>entries.len(),"entries":entries,"historical_context":true})
}
#[cfg(test)]
mod tests;
