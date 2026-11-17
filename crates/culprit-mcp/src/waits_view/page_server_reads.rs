//! Compact transport of one shared explanation; captured history and live scan plan statistics remain separate.
use culprit_core::{
    Report, Result, SourceState,
    i18n::Message,
    page_server_read_report::{PageServerReadEntry, PageServerReadReport},
    query_store_statement_report::QueryStoreStatementBinding,
    telemetry::{CapturedReadRef, DeliveryProblem, digest},
};
use serde::Serialize;
use serde_json::{Value, json};

const ENTRY_LIMIT: usize = 5;

enum Origin<'a> {
    Scan(&'a str),
    Capture(&'a CapturedReadRef),
}
impl Origin<'_> {
    fn source_reference(&self, source: &str) -> Option<String> {
        match self {
            Self::Scan(id) => reference("page-server-source-v1", &(*id, source)),
            Self::Capture(read) => reference("page-server-captured-source-v1", read),
        }
    }
    fn entry_reference(&self, entry: &PageServerReadEntry) -> Option<String> {
        match self {
            Self::Scan(id) => reference("page-server-entry-v1", &(*id, &entry.identity)),
            Self::Capture(read) => reference("page-server-captured-entry-v1", &(read, &entry.identity)),
        }
    }
}

pub(super) fn attach(response: &mut Value, report: &Report) {
    let view = match culprit_engine::explain_page_server_reads(&report.snapshot) {
        Ok(Some(view)) => view,
        _ => {
            unavailable(response, report);
            return;
        }
    };
    let Some(mut compact) = compact(&view, Origin::Scan(&report.snapshot.id)) else {
        unavailable(response, report);
        return;
    };
    compact["definition_evidence_unavailable"] = json!(
        report.page_server_reads.as_ref().is_some_and(|previous| previous.definition_read.is_some())
            && view.definition_read.is_none()
    );
    response["page_server_reads"] = compact;
}

/// Only the private original-capture owner can construct this input. No cached derived report is accepted here.
pub(crate) fn captured(analysis: &culprit_engine::LocalPageServerReads) -> Result<Value> {
    let view = analysis.report();
    let read = view.runtime_read.as_deref().filter(|read| read.is_valid()).ok_or_else(invalid)?;
    let view = compact(view, Origin::Capture(read)).ok_or_else(invalid)?;
    Ok(json!({
        "page_server_reads": view,
        "native_history_window": {"from": analysis.history_from(), "until": analysis.history_until()},
        "runtime_population_membership": analysis.population_membership(),
        "collected_from": analysis.collected_from(), "collected_until": analysis.collected_until(),
        "local_verification_bytes": analysis.bytes_read(),
    }))
}

fn compact(view: &PageServerReadReport, origin: Origin<'_>) -> Option<Value> {
    let source = origin.source_reference(&view.source)?;
    let entries =
        view.entries.iter().take(ENTRY_LIMIT).map(|entry| point(&origin, entry)).collect::<Option<Vec<_>>>()?;
    Some(json!({
        "title": view.title, "summary": view.summary, "availability": view.availability, "source_reference": source,
        "source_state": view.source_state, "read_complete": view.read_complete,
        "partial": !view.read_complete || view.source_state == Some(SourceState::Partial),
        "selected_rows": view.selected_rows, "observed_rows": view.observed_rows,
        "references_complete": view.reference_coverage.as_ref().map(|coverage|coverage.complete),
        "reference_integrity_complete": view.reference_coverage.as_ref().map(|coverage|coverage.integrity_complete),
        "query_references": view.reference_coverage.as_ref().map(|coverage|&coverage.queries),
        "plan_references": view.reference_coverage.as_ref().map(|coverage|&coverage.plans),
        "presentation_limited": view.presentation_limited, "entries_retained": view.entries.len(), "entries_presented": entries.len(),
        "entries_limited": view.presentation_limited || view.entries.len() > ENTRY_LIMIT, "historical_context": true,
        "entries": entries, "limitations": view.limitations, "definition_read": view.definition_read,
    }))
}

fn point(origin: &Origin<'_>, entry: &PageServerReadEntry) -> Option<Value> {
    let mut value = json!({
        "reference": origin.entry_reference(entry)?,
        "executions": entry.executions, "availability": entry.explanation.availability,
        "statement_binding": entry.statement.as_ref().map(|statement|statement.binding),
        "statement_origin": entry.statement.as_ref().map(|statement|statement.origin),
        "statement_payload_unavailable": entry.statement.as_ref().and_then(|statement|statement.unavailable),
        "title": entry.explanation.title, "summary": entry.explanation.summary,
        "metrics": entry.explanation.metrics.iter().map(|metric| json!({ "key": metric.key, "label": metric.label, "text": metric.text })).collect::<Vec<_>>(),
        "limitations": entry.explanation.limitations, "investigate": entry.explanation.investigate, "verify": entry.explanation.verify,
    });
    if matches!(origin, Origin::Capture(_)) {
        let statement =
            entry.statement.as_ref().filter(|statement| statement.binding == QueryStoreStatementBinding::Matched);
        // The original binder owns SQL redaction and its shared 512-character preview limit.
        value["sql_preview"] = json!(statement.and_then(|statement| statement.preview.as_ref()));
        value["sql_preview_truncated"] = json!(statement.is_some_and(|statement| statement.preview_truncated));
        if let culprit_core::page_server_read_report::PageServerReadEntryIdentity::RuntimeFragment {
            reference, ..
        } = &entry.identity
        {
            value["informational"] = json!(reference.is_informational());
        }
    }
    Some(value)
}

fn invalid() -> culprit_core::Error {
    DeliveryProblem::Integrity.error()
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
