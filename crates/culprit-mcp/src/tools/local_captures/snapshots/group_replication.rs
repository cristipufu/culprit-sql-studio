//! The local owner adds references only after exact original membership authentication.
use culprit_core::GroupReplicationReport;
use culprit_engine::LocalSnapshotAnalysis;
use serde_json::{Value, json};
pub(super) fn value(analysis: &LocalSnapshotAnalysis, report: &GroupReplicationReport) -> Value {
    let mut value = crate::waits_view::group_replication::value(report);
    if let Some(entries) = value["entries"].as_array_mut() {
        for (entry, original) in entries.iter_mut().zip(&report.entries) {
            entry["reference"] = json!(super::reference(analysis, &original.reference.observation_key));
        }
    }
    value
}
