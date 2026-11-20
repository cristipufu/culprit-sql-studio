//! Human output delegates meaning and bounds to the existing report and shared observation formatter.
use super::{
    SourceQualification, Summary, append_remaining_limitations, native_metrics, native_work_summary, statement,
};
use culprit_core::TableMemoryReport;
pub(crate) fn table_memory(report: &TableMemoryReport) -> String {
    let target = report.original.as_ref().map(|original| {
        format!("{}.{}", statement::preview(&original.metadata.schema), statement::preview(&original.metadata.name))
    });
    let mut text = native_work_summary(
        Summary {
            title: &report.title,
            summary: &report.summary,
            source: SourceQualification::Single {
                state: Some(report.source.state),
                reason: report.source.reason.as_deref(),
            },
            limitations: &report.limitations,
        },
        target.as_deref().into_iter().map(|target| (target, native_metrics(&report.metrics).collect::<Vec<_>>())),
    );
    append_remaining_limitations(&mut text, &report.limitations);
    text
}
#[cfg(test)]
mod tests;

pub(crate) fn compact(report: &TableMemoryReport) -> serde_json::Value {
    serde_json::json!({"title":report.title,"summary":report.summary,"source":report.source,"metrics":report.metrics,"limitations":report.limitations,
        "read_complete":report.read_complete,"native_rows":report.native_rows,"observed_rows":report.observed_rows,
        "selected_target":report.original.as_ref().map(|original|serde_json::json!({"database_id":original.target.database_id,"object_id":original.target.object_id,"schema":original.metadata.schema,"table":original.metadata.name})),
        "original_read_bounds":report.original.as_ref().map(|original|serde_json::json!({"from":original.context.observed_from,"until":original.context.observed_until}))})
}
