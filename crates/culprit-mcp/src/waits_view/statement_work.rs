//! The shared report owns interpretation; MCP bounds presentation and withholds private native identities.
use culprit_core::{Report, ResetMarker, StatementWorkEntry, telemetry::digest};
use serde::Serialize;
use serde_json::{Value, json};

const ENTRY_LIMIT: usize = 5;

pub(super) fn attach(response: &mut Value, report: &Report) {
    let Some(native) = report.snapshot.operations.statement_work.as_deref() else { return };
    let Some(explained) =
        report.statement_work.as_deref().filter(|view| view.source == native.source && view.context == native.context)
    else {
        response["statement_work_unavailable"] = json!(culprit_core::t!("view.statement_work.invalid"));
        return;
    };
    response["statement_work"] = json!({
        "title": explained.title, "summary": explained.summary,
        "source_reference": reference("statement-counter-source-v1", &explained.source),
        "source_state": explained.source_state, "source_reason": explained.source_reason,
        "source_error_code": native.coverage.error_code, "collection_stopped": native.coverage.stopped,
        "complete_scope": native.coverage.complete_scope,
        "database": native.context.as_deref().and_then(|context| context.target.database()).map(|database| &database.database),
        "read_bounds": native.context.as_deref().map(|context| json!({
            "observed_from": context.observed_from, "observed_until": context.observed_until,
        })),
        "native_rows": native.coverage.records, "observed_rows": native.observations.len(),
        "entries_retained": explained.entries.len(), "entries_presented": explained.entries.len().min(ENTRY_LIMIT),
        "entries_limited": explained.entries.len() > ENTRY_LIMIT,
        "entries": explained.entries.iter().take(ENTRY_LIMIT).map(|entry| point(&report.snapshot.id, entry)).collect::<Vec<_>>(),
        "limitations": explained.limitations,
    });
}

fn point(snapshot_id: &str, entry: &StatementWorkEntry) -> Value {
    json!({
        "reference": reference("statement-counter-point-v1", &(snapshot_id, &entry.source, &entry.observation_key)),
        "scope": entry.scope, "observed_at": entry.observed_at, "reset": reset(&entry.reset),
        "outcomes": entry.outcomes, "outcome_metrics": entry.outcome_metrics, "work": entry.work,
    })
}

fn reset(reset: &ResetMarker) -> Value {
    match reset {
        ResetMarker::Unknown => json!({ "kind": "unknown" }),
        ResetMarker::At(at) => json!({ "kind": "at", "value": at }),
        ResetMarker::Generation(generation) => json!({ "kind": "generation",
            "reference": reference("statement-counter-reset-v1", generation) }),
    }
}

fn reference(domain: &str, value: &impl Serialize) -> String {
    digest(&serde_json::to_vec(&(domain, value)).expect("native statement references serialize"))
}

#[cfg(test)]
mod tests;
