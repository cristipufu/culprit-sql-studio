use super::{SourceQualification, Summary, append_remaining_limitations, native_metrics, native_work_summary};
use culprit_core::{format, query_runtime_report::QueryRuntimeFragmentReport, t};
pub(crate) fn runtime_fragments(report: &QueryRuntimeFragmentReport) -> String {
    let entries = report
        .entries
        .iter()
        .map(|entry| {
            let title = t!("view.runtime_fragments.entry", executions = format::digits(entry.executions.clone()));
            let mut fields = entry
                .metrics
                .iter()
                .flat_map(|metric| {
                    native_metrics(&metric.statistics).map(|value| format!("{}: {}", metric.title, value))
                })
                .collect::<Vec<_>>();
            if let Some(pages) = &entry.page_server_explanation {
                fields.extend(native_metrics(&pages.metrics));
            }
            (title, fields)
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
        entries.iter().map(|(title, fields)| (title.as_str(), fields.clone())),
    );
    append_remaining_limitations(&mut text, &report.limitations);
    text
}
pub(crate) fn compact_runtime_fragments(report: &QueryRuntimeFragmentReport) -> serde_json::Value {
    let entries=report.entries.iter().take(5).map(|entry|serde_json::json!({"executions":entry.executions,"first_execution_at":entry.first_execution_at,"last_execution_at":entry.last_execution_at,"metrics":entry.metrics,"page_server_reads":entry.page_server_reads,"page_server_explanation":entry.page_server_explanation})).collect::<Vec<_>>();
    serde_json::json!({"title":report.title,"summary":report.summary,"source_state":report.source_state,"source_reason":report.source_reason,"read_complete":report.read_complete,"references_complete":report.references_complete,"selected_rows":report.selected_rows,"observed_rows":report.observed_rows,"entries_presented":entries.len(),"entries_limited":report.presentation_limited || report.entries.len()>entries.len(),"entries":entries,"limitations":report.limitations,"historical_context":true})
}
#[cfg(test)]
mod tests;
