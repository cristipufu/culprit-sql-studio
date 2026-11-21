//! One bounded public wait view for saved scans and authenticated local originals.
use culprit_core::{Report, WaitCounterReport};
use serde_json::{Value, json};
const ENTRY_LIMIT: usize = 5;
pub(super) fn attach(response: &mut Value, report: &Report) {
    if let Some(waits) = &report.wait_counters {
        response["wait_counters"] = value(waits);
    } else if report.snapshot.pressure.as_ref().is_some_and(|pressure| pressure.wait_counters.is_some())
        || report.snapshot.source_problem(&[culprit_core::source_id::WAITS]).is_some()
    {
        response["wait_counters_unavailable"] = json!(culprit_core::t!("view.wait_counters.invalid"));
    }
}
pub(crate) fn value(report: &WaitCounterReport) -> Value {
    let bounds = report.context.as_ref().map(|context| {
        let (from, until) = context.bounds();
        json!({"from":from,"until":until})
    });
    json!({"title":report.title,"summary":report.summary,"sources":report.sources,"read_complete":report.read_complete,
        "native_rows":report.native_rows,"observed_rows":report.observed_rows,"presented_rows":report.entries.len().min(ENTRY_LIMIT),
        "entries_limited":report.entries_limited || report.entries.len()>ENTRY_LIMIT,"original_read_bounds":bounds,
        "limitations":report.limitations,"entries":report.entries.iter().take(ENTRY_LIMIT).map(|entry|json!({
            "wait_type":entry.observation.wait_type,"source":entry.observation.source,"scope":entry.observation.scope,
            "reset":entry.observation.reset,"waiting_tasks_count":entry.observation.waiting_tasks_count,
            "wait_time_ms":entry.observation.wait_time_ms,"signal_wait_time_ms":entry.observation.signal_wait_time_ms,
            "max_wait_time_ms":entry.observation.max_wait_time_ms,"resource_wait_time_ms":entry.resource_wait_time_ms,
            "title":entry.title,"explanation":entry.explanation,"metrics":entry.metrics,"issues":entry.issues
        })).collect::<Vec<_>>()})
}

#[cfg(test)]
mod tests;
