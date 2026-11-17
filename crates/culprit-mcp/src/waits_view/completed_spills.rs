//! A compact projection of the canonical shared view, never a second cache reader or native-owner join.
use culprit_core::{CompletedSpillEntry, CompletedSpillReference, Report, Result, SourceOutcome, telemetry::*};
use serde_json::{Value, json};

const ENTRY_LIMIT: usize = 5;

pub(super) fn attach(response: &mut Value, report: &Report) {
    let Ok(Some(view)) = culprit_engine::explain_completed_spills(&report.snapshot) else {
        unavailable(response, report);
        return;
    };
    let Ok(entries) = view.entries.iter().take(ENTRY_LIMIT).map(point).collect::<Result<Vec<_>>>() else {
        unavailable(response, report);
        return;
    };
    response["completed_spills"] = json!({
        "title": view.title, "summary": view.summary,
        "sources": sources(&view.sources), "read_complete": view.read_complete,
        "read_bounds": { "observed_from": view.context.observed_from, "observed_until": view.context.observed_until },
        "native_rows": view.native_rows, "observed_rows": view.observed_rows,
        "definition_read_complete": view.definition_read_complete,
        "definition_read_bounds": view.definition_context.as_ref().map(|context| json!({
            "observed_from": context.observed_from, "observed_until": context.observed_until,
        })),
        "definition_sources": sources(&view.definition_sources),
        "entries_retained": view.entries.len(), "entries_presented": entries.len(),
        "entries_limited": view.entries.len() > ENTRY_LIMIT,
        "entries": entries, "limitations": view.limitations,
    });
}

fn unavailable(response: &mut Value, report: &Report) {
    let gaps = report
        .snapshot
        .sources
        .iter()
        .filter(|source| source.source == CACHED_RUNTIME_READ_SOURCE)
        .cloned()
        .collect::<Vec<_>>();
    if report.snapshot.operations.cached_runtime.is_some() || report.completed_spills.is_some() || !gaps.is_empty() {
        response["completed_spills_unavailable"] = json!({
            "reason": culprit_core::t!("view.completed_spills.invalid"), "sources": sources(&gaps),
        });
    }
}

fn point(entry: &CompletedSpillEntry) -> Result<Value> {
    Ok(json!({
        "reference": reference(&entry.observation)?,
        "definition_reference": entry.definition.as_ref().map(reference).transpose()?,
        "state": entry.state, "origin": entry.origin, "monitoring_template_id": entry.monitoring_template_id,
        "statement_binding": entry.statement_binding, "statement_unavailable": entry.statement_unavailable,
        "statement_preview_truncated": entry.statement_preview_truncated,
        "title": entry.title, "explanation": entry.explanation, "metrics": entry.metrics,
        "investigation": entry.investigation, "verify": entry.verify,
    }))
}

fn reference(reference: &CompletedSpillReference) -> Result<Value> {
    if !reference.is_valid() {
        return Err(DeliveryProblem::Integrity.error());
    }
    Ok(match reference {
        CompletedSpillReference::NativeRead { read_key, observation_key } => json!({
            "kind": "native-read", "read_reference": read_key, "observation_reference": observation_key,
        }),
        CompletedSpillReference::Captured { reference } => json!({ "kind": "captured", "reference": reference.key()? }),
    })
}

fn sources(sources: &[SourceOutcome]) -> Vec<Value> {
    sources.iter().map(|source| json!({
        "reference": digest(format!("completed-spill-source-v1:{}", source.source).as_bytes()), "state": source.state,
    })).collect()
}

#[cfg(test)]
mod tests;
