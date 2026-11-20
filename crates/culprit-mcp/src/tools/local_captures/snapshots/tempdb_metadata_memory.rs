//! The already-qualified report supplies values; presentation neither joins raw rows nor exports native proof.
use culprit_core::{TempdbMetadataMemoryEntry, TempdbMetadataMemoryReport};
use culprit_engine::LocalSnapshotAnalysis;
use serde_json::{Value, json};

pub(super) fn value(analysis: &LocalSnapshotAnalysis, report: &TempdbMetadataMemoryReport) -> Value {
    let entries = report
        .entries
        .iter()
        .take(super::ENTRY_LIMIT)
        .map(|entry| consumer(entry, super::reference(analysis, &entry.observation_key)))
        .collect();
    let mut value = super::summary(
        &report.title,
        &report.summary,
        Some(report.source_state),
        &None,
        &report.limitations,
        report.entries.len(),
        entries,
    );
    value["native_source"] = json!({
        "state":report.native_source.state,"rows":report.native_source.records,
        "complete_scope":report.native_source.complete_scope,"error_code":report.native_source.error_code,
        "stopped":report.native_source.stopped,
    });
    value["native_rows"] = json!(report.native_rows);
    value["observed_rows"] = json!(report.observed_rows);
    value["read_complete"] = json!(report.read_complete);
    value["metadata_enabled"] = json!(report.metadata_enabled);
    value["feature_observed_at"] = json!(report.feature_observed_at);
    value["tempdb_created_without_offset"] = json!(report.tempdb_created_without_offset);
    value
}

fn consumer(entry: &TempdbMetadataMemoryEntry, reference: String) -> Value {
    json!({
        "reference":reference,"observed_at":entry.observed_at,
        "native_consumer_id":entry.native_consumer_id,"native_type_code":entry.native_type_code,
        "native_type_name":entry.native_type_name,"description":entry.description,
        "allocated_bytes":entry.allocated_bytes,"used_bytes":entry.used_bytes,
        "explanation":entry.explanation,"investigation":entry.investigation,"verify":entry.verify,
    })
}

#[cfg(test)]
mod tests;
