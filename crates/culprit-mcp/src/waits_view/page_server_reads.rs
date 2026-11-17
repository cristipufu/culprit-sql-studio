//! Compact transport of shared reanalysis; no SQL, native proof internals or derived-report authority.
use culprit_core::{Report, Snapshot, i18n::Message, page_server_read_report::PageServerReadEntry, telemetry::digest};
use serde::Serialize;
use serde_json::{Value, json};

const ENTRY_LIMIT: usize = 5;

pub(super) fn attach(response: &mut Value, report: &Report) {
    let view = match culprit_engine::explain_page_server_reads(&report.snapshot) {
        Ok(Some(view)) => view,
        _ => {
            unavailable(response, report);
            return;
        }
    };
    let Some(source) = reference("page-server-source-v1", &(&report.snapshot.id, &view.source)) else {
        unavailable(response, report);
        return;
    };
    let entries =
        view.entries.iter().take(ENTRY_LIMIT).map(|entry| point(&report.snapshot, entry)).collect::<Option<Vec<_>>>();
    let Some(entries) = entries else {
        unavailable(response, report);
        return;
    };
    response["page_server_reads"] = json!({
        "title": view.title, "summary": view.summary, "availability": view.availability, "source_reference": source,
        "source_state": view.source_state, "read_complete": view.read_complete, "partial": !view.read_complete,
        "selected_rows": view.selected_rows, "observed_rows": view.observed_rows,
        "presentation_limited": view.presentation_limited, "entries_retained": view.entries.len(), "entries_presented": entries.len(),
        "entries_limited": view.presentation_limited || view.entries.len() > ENTRY_LIMIT, "historical_context": true,
        "entries": entries, "limitations": view.limitations,
        "definition_read": view.definition_read,
        "definition_evidence_unavailable": report.page_server_reads.as_ref().is_some_and(|previous|previous.definition_read.is_some()) && view.definition_read.is_none(),
    });
}

fn point(snapshot: &Snapshot, entry: &PageServerReadEntry) -> Option<Value> {
    Some(json!({
        "reference": reference("page-server-entry-v1", &(&snapshot.id, &entry.identity))?,
        "executions": entry.executions, "availability": entry.explanation.availability,
        "statement_binding": entry.statement.as_ref().map(|statement|statement.binding),
        "statement_origin": entry.statement.as_ref().map(|statement|statement.origin),
        "statement_payload_unavailable": entry.statement.as_ref().and_then(|statement|statement.unavailable),
        "title": entry.explanation.title, "summary": entry.explanation.summary,
        "metrics": entry.explanation.metrics.iter().map(|metric| json!({ "key": metric.key, "label": metric.label, "text": metric.text })).collect::<Vec<_>>(),
        "limitations": entry.explanation.limitations, "investigate": entry.explanation.investigate, "verify": entry.explanation.verify,
    }))
}

fn unavailable(response: &mut Value, report: &Report) {
    let raw = report.snapshot.workload.as_ref().is_some_and(|workload| {
        workload.queries.iter().flat_map(|query| &query.plans).any(|plan| plan.page_server_reads.is_some())
    });
    let marker = report.snapshot.sources.iter().any(|source| {
        source.source == culprit_core::source_id::QUERY_STORE
            && matches!(source.reason_message.as_ref(), Some(Message::Catalog { key, arguments })
                if key == "view.page_server_reads.invalid" && arguments.is_empty())
    });
    if raw || marker || report.page_server_reads.is_some() {
        response["page_server_reads_unavailable"] = json!({
            "reason": culprit_core::t!("view.page_server_reads.invalid"), "read_complete": false, "raw_evidence_present": raw,
        });
    }
}

fn reference(domain: &str, value: &impl Serialize) -> Option<String> {
    serde_json::to_vec(&(domain, value)).ok().map(|bytes| digest(&bytes))
}

#[cfg(test)]
mod tests;
