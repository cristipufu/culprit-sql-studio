//! The selected target and canonical metrics are historical capture data, without native source proof payloads.
use culprit_core::ColumnstoreRowgroupReport;
use culprit_engine::LocalSnapshotAnalysis;
use serde_json::{Value, json};
pub(super) fn value(analysis: &LocalSnapshotAnalysis, report: &ColumnstoreRowgroupReport) -> Value {
    let entries = report
        .entries
        .iter()
        .take(super::ENTRY_LIMIT)
        .map(|entry| {
            json!({
                "reference":super::reference(analysis,&entry.observation_key),"observed_at":entry.observed_at,
                "partition_number":entry.partition_number,"row_group_id":entry.row_group_id,"state":entry.state,
                "title":entry.title,"metrics":entry.metrics,"investigation":report.investigation,"verify":report.verify,
            })
        })
        .collect();
    let mut value = super::summary(
        &report.title,
        &report.summary,
        Some(report.source.state),
        &report.source.reason,
        &report.limitations,
        report.entries.len(),
        entries,
    );
    value["selected_target"] = json!({"database_id":report.target.table.database_id,"object_id":report.target.table.object_id,
        "index_id":report.target.index_id,"partition_number":report.target.partition_number});
    value["native_rows"] = json!(report.native_rows);
    value["observed_rows"] = json!(report.observed_rows);
    value["read_complete"] = json!(report.read_complete);
    value
}
