use culprit_core::CdcReport;
use culprit_engine::LocalSnapshotAnalysis;
use serde_json::{Value, json};
pub(super) fn value(analysis: &LocalSnapshotAnalysis, report: &CdcReport) -> Value {
    let mut value = super::change_capture::value(
        analysis,
        &report.title,
        &report.summary,
        report.source_state,
        &report.limitations,
        &report.entries,
    );
    value["enabled"] = json!(report.enabled);
    value["read_complete"] = json!(report.read_complete);
    value
}
