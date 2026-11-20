//! Bounded canonical CDC output. No private original scope, LSN, native message or receipt is serialized here.
use culprit_core::CdcReport;
use culprit_engine::LocalSnapshotAnalysis;
use serde_json::{Value, json};

const DISPLAY_FIELDS: usize = 8;
pub(super) fn value(analysis: &LocalSnapshotAnalysis, report: &CdcReport) -> Value {
    let entries = report.entries.iter().take(super::ENTRY_LIMIT).map(|entry| json!({
        "reference":super::reference(analysis, &entry.observation_key),
        "kind":entry.kind,"observed_at":entry.observed_at,"title":entry.title,"explanation":entry.explanation,
        "fields":entry.fields.iter().take(DISPLAY_FIELDS).map(|field|json!({"key":field.key,"label":field.label,"value":field.value})).collect::<Vec<_>>(),
        "investigation":entry.investigation,"verify":entry.verify,
    })).collect();
    let mut value = super::summary(
        &report.title,
        &report.summary,
        Some(report.source_state),
        &None,
        &report.limitations,
        report.entries.len(),
        entries,
    );
    value["enabled"] = json!(report.enabled);
    value["read_complete"] = json!(report.read_complete);
    value
}
