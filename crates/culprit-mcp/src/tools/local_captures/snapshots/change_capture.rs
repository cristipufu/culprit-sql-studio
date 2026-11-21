//! Compact original log-capture context omits raw owners, LSNs, receipts and native private error text.
use culprit_core::{ChangeCaptureEntry, SourceState};
use culprit_engine::LocalSnapshotAnalysis;
use serde_json::{Value, json};
const DISPLAY_FIELDS: usize = 8;
pub(super) fn value(
    analysis: &LocalSnapshotAnalysis,
    title: &str,
    summary: &str,
    state: SourceState,
    limitations: &[String],
    entries: &[ChangeCaptureEntry],
) -> Value {
    let displayed_entries = entries.iter().take(super::ENTRY_LIMIT).map(|entry| json!({
        "reference":super::reference(analysis, &entry.observation_key),
        "kind":entry.kind,"observed_at":entry.observed_at,"title":entry.title,"explanation":entry.explanation,
        "fields_available":entry.fields.len(),"fields_omitted":entry.fields.len().saturating_sub(DISPLAY_FIELDS),
        "fields":entry.fields.iter().take(DISPLAY_FIELDS).map(|field|json!({"key":field.key,"label":field.label,"value":field.value})).collect::<Vec<_>>(),
        "investigation":entry.investigation,"verify":entry.verify,
    })).collect();
    let value = super::summary(title, summary, Some(state), &None, limitations, entries.len(), displayed_entries);
    value
}
