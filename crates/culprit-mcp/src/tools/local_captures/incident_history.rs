//! A bounded historical projection only; raw endpoints, native actor keys and live actions are withheld.
use super::*;
use culprit_engine::LocalIncidentHistory;
const ENTRY_LIMIT: usize = 5;
pub(super) fn captured(analysis: &LocalIncidentHistory) -> Result<Value> {
    let mut result = value(analysis.report(), analysis.bytes_read())?;
    result["collected_from"] = json!(analysis.collected_from());
    result["collected_until"] = json!(analysis.collected_until());
    Ok(result)
}
pub(super) fn value(report: &culprit_core::IncidentHistoryReport, bytes_read: u64) -> Result<Value> {
    let entries = report.entries.iter().take(ENTRY_LIMIT).map(|entry| {
        let evidence = entry.evidence.iter().map(|evidence| Ok(json!({
            "label":evidence.label, "availability":evidence.availability, "capture_reference":evidence.capture.key()?,
        }))).collect::<Result<Vec<_>>>()?;
        Ok(json!({
            "controller_version":entry.controller_version,"revision_id":entry.revision_id, "episode_id":entry.episode_id, "target_id":entry.target_id,"target_scope":entry.target_scope,"scope_id":entry.scope_id,
            "scope":entry.scope,"scope_label":entry.scope_label, "revision":entry.revision, "previous_revision":entry.previous_revision,
            "predecessor_selected":entry.predecessor_selected, "lifecycle":entry.lifecycle, "analysis":entry.analysis,
            "title":entry.title, "phase":entry.phase, "first_seen_at":entry.first_seen_at,
            "last_seen_at":entry.last_seen_at, "recorded_at":entry.recorded_at,
            "original":entry.original, "input_references":entry.input_references,
            "followup_recipe":entry.followup_recipe,"followup_recipe_version":entry.followup_recipe_version,
            "evidence":evidence, "issue":entry.issue, "reasons":entry.reasons, "next_step":entry.next_step,
        }))
    }).collect::<Result<Vec<_>>>()?;
    Ok(json!({"incident_history": {
        "title": report.title, "summary": report.summary, "limitations": report.limitations,
        "provenance": report.provenance, "selected_read_complete": report.selected_read_complete,
        "source_state": report.source_state, "source_reason": report.source_reason,
        "entries_available": report.entries.len(), "entries_presented": report.entries.len().min(ENTRY_LIMIT),
        "entries_omitted": report.entries.len().saturating_sub(ENTRY_LIMIT),
        "entries": entries
    }, "local_verification_bytes":bytes_read}))
}
#[cfg(test)]
mod tests;
