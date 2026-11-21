use culprit_core::{
    Result,
    query_runtime_report::QueryRuntimeFragmentReport,
    telemetry::{DeliveryProblem, digest},
};
use serde_json::{Value, json};
const ENTRY_LIMIT: usize = 5;
pub(crate) fn value(report: &QueryRuntimeFragmentReport) -> Result<Value> {
    let entries=report.entries.iter().take(ENTRY_LIMIT).map(|entry| {
        let read=report.read.as_deref().ok_or_else(||DeliveryProblem::Integrity.error())?;
        let bytes=serde_json::to_vec(&("runtime-fragment-v1",read,&entry.reference)).map_err(|_|DeliveryProblem::Integrity.error())?;
        Ok(json!({"reference":digest(&bytes),"informational":entry.reference.is_informational(),"executions":entry.executions,"first_execution_at":entry.first_execution_at,"last_execution_at":entry.last_execution_at,"metrics":entry.metrics,"page_server_reads":entry.page_server_reads,"page_server_explanation":entry.page_server_explanation}))
    }).collect::<Result<Vec<_>>>()?;
    Ok(
        json!({"title":report.title,"summary":report.summary,"source_state":report.source_state,"source_reason":report.source_reason,"read_complete":report.read_complete,"references_complete":report.references_complete,"selected_rows":report.selected_rows,"observed_rows":report.observed_rows,"entries_presented":entries.len(),"entries_limited":report.presentation_limited||report.entries.len()>entries.len(),"entries":entries,"limitations":report.limitations,"collected_from":report.collected_from,"collected_until":report.collected_until,"historical_context":true}),
    )
}
pub(super) fn attach(response: &mut Value, report: &culprit_core::Report) {
    let invalid = || {
        culprit_engine::query_runtime_fragments::unavailable(&culprit_core::message!("view.runtime_fragments.invalid"))
    };
    let view = match culprit_engine::query_runtime_fragments::explain_snapshot(&report.snapshot) {
        Ok(Some(view)) => view,
        Ok(None) if report.runtime_fragments.is_none() => return,
        Ok(None) | Err(_) => invalid(),
    };
    response["runtime_fragments"] = value(&view).unwrap_or_else(|_| value(&invalid()).unwrap_or(Value::Null));
}
