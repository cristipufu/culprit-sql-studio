use culprit_core::{
    Result,
    query_wait_report::QueryWaitFragmentReport,
    telemetry::{DeliveryProblem, digest},
};
use serde_json::{Value, json};
const ENTRY_LIMIT: usize = 5;

pub(crate) fn value(report: &QueryWaitFragmentReport) -> Result<Value> {
    let entries = report.entries.iter().take(ENTRY_LIMIT).map(|entry| {
        let read = report.read.as_deref().ok_or_else(|| DeliveryProblem::Integrity.error())?;
        let bytes = serde_json::to_vec(&("query-wait-fragment-v1", read, &entry.reference)).map_err(|_| DeliveryProblem::Integrity.error())?;
        Ok(json!({"reference":digest(&bytes),"category":entry.category,"category_description":entry.category_description,"outcome_description":entry.outcome_description,"unit":entry.unit,
            "informational":entry.reference.is_informational(),"metrics":entry.metrics}))
    }).collect::<Result<Vec<_>>>()?;
    Ok(
        json!({"title":report.title,"summary":report.summary,"source_state":report.source_state,"source_reason":report.source_reason,
        "selected_rows":report.selected_rows,"observed_rows":report.observed_rows,"capture":report.capture,"interval_coverage":report.interval_coverage,"references_complete":report.references_complete,"read_complete":report.read_complete,
        "entries_available":report.observed_rows,"entries_presented":entries.len(),"entries_limited":report.presentation_limited || report.entries.len()>entries.len(),
        "entries":entries,"limitations":report.limitations,"collected_from":report.collected_from,"collected_until":report.collected_until,"historical_context":true}),
    )
}
pub(super) fn attach(response: &mut Value, report: &culprit_core::Report) {
    let invalid = || {
        culprit_engine::query_wait_fragments::unavailable(&culprit_core::message!("view.query_wait_fragments.invalid"))
    };
    let view = match culprit_engine::query_wait_fragments::explain_snapshot(&report.snapshot) {
        Ok(Some(view)) => view,
        Ok(None) if report.query_wait_fragments.is_none() => return,
        Ok(None) | Err(_) => invalid(),
    };
    response["query_wait_fragments"] = value(&view).unwrap_or_else(|_| {
        // A malformed derived reference never becomes a silently omitted or available public entry.
        value(&invalid()).unwrap_or(Value::Null)
    });
}
