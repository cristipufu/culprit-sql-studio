//! Compact local evidence keeps original scope/counts while withholding private capture/source proof internals.
use culprit_core::{Result, telemetry::DeliveryProblem};
use culprit_engine::LocalMemoryClerks;
use serde_json::{Value, json};

const ENTRY_LIMIT: usize = 5;

pub(super) fn captured(analysis: &LocalMemoryClerks) -> Result<Value> {
    let read = analysis.original().read();
    let view = analysis.report();
    // The opaque owner already authenticated and interpreted the original body. Presentation does no second pass.
    let source = view.native_source.as_ref().ok_or_else(|| DeliveryProblem::Integrity.error())?;
    let context =
        view.context.as_ref().and_then(|context| context.native()).ok_or_else(|| DeliveryProblem::Integrity.error())?;
    let entries = view
        .entries
        .iter()
        .take(ENTRY_LIMIT)
        .map(|entry| {
            let reference = culprit_core::telemetry::digest(
                &serde_json::to_vec(&("local-memory-clerk-entry", &read.origin.capture, &entry.observation_key))
                    .map_err(|_| DeliveryProblem::Integrity.error())?,
            );
            Ok(json!({
                "reference": reference,
                "scope": entry.scope, "clerk_type": entry.clerk_type, "name": entry.name,
                "memory_node_id": entry.memory_node_id, "explanation": entry.explanation,
                "fields": entry.fields.iter().map(|field| json!({
                    "key": field.key, "label": field.label, "value": field.value, "unit": field.unit,
                })).collect::<Vec<_>>(),
            }))
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(json!({
        "memory_clerks": {
            "title": view.title, "summary": view.summary,
            "source_state": view.source_state, "source_reason": view.source_reason,
            "native_source_state": source.state, "native_rows": view.native_rows,
            "read_complete": view.read_complete, "observed_rows": view.observed_rows,
            "backend_presented_rows": view.presented_rows, "entries_presented": entries.len(),
            "entries_limited": view.entries_limited || view.entries.len() > ENTRY_LIMIT,
            "original_read_bounds": {"from": context.observed_from, "until": context.observed_until},
            "historical_context": true, "independent_shared_scope": true,
            "limitations": view.limitations, "entries": entries,
        },
        "collected_from": analysis.original().collected_from(), "collected_until": analysis.original().collected_until(),
        "local_verification_bytes": analysis.original().bytes_read(),
    }))
}
