use culprit_core::ChangeFeedReport;
use culprit_engine::LocalSnapshotAnalysis;
use serde_json::{Value, json};
pub(super) fn value(analysis: &LocalSnapshotAnalysis, report: &ChangeFeedReport) -> Value {
    let mut value = super::change_capture::value(
        analysis,
        &report.title,
        &report.summary,
        report.source_state,
        &report.limitations,
        &report.entries,
    );
    value["read_complete"] = json!(report.read_complete);
    value
}
