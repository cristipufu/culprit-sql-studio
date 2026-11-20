//! Bounded historical context; no native ownership/proof payload or current catalog enrichment.
use culprit_core::FullTextPopulationReport;
use culprit_engine::LocalSnapshotAnalysis;
use serde_json::{Value, json};

pub(super) fn value(analysis: &LocalSnapshotAnalysis, report: &FullTextPopulationReport) -> Value {
    let entries = report
        .entries
        .iter()
        .take(super::ENTRY_LIMIT)
        .map(|entry| {
            json!({
                "reference":super::reference(analysis,&entry.observation_key),"observed_at":entry.observed_at,
                "position":entry.position,"status":entry.status,"summary":entry.summary,"metrics":entry.metrics,
                "investigation":report.investigation,"verify":report.verify,
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
    if let Some(original) = &report.original {
        let target = &original.target;
        value["observed_at"] = json!(original.metadata.observed_at);
        value["selected_target"] = json!({"database_id":target.table.database_id,"object_id":target.table.object_id,
            "catalog_id":target.catalog_id,"unique_index_id":target.unique_index_id,"schema":original.metadata.schema,"table":original.metadata.table});
        value["observed_from"] = json!(original.context.observed_from);
        value["observed_until"] = json!(original.context.observed_until);
    }
    value["native_rows"] = json!(report.native_rows);
    value["observed_rows"] = json!(report.observed_rows);
    value["read_complete"] = json!(report.read_complete);
    value
}
