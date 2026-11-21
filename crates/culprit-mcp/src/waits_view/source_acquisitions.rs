//! The allowlist is the same sanitized metadata view for direct inspection and historical saved results.
use culprit_core::HistoricalSourceOverhead;
use serde_json::{Value, json};
pub(crate) fn value(report: &HistoricalSourceOverhead) -> Value {
    match report {
        HistoricalSourceOverhead::Available { report, .. } => json!({
            "kind":"available", "report":report, "historical_context":true,
        }),
        HistoricalSourceOverhead::Unavailable { reason } => json!({
            "kind":"unavailable", "reason":reason, "historical_context":true,
        }),
    }
}
pub(super) fn attach(response: &mut Value, report: &culprit_core::Report) {
    let mut original = culprit_engine::source_overhead::explain_snapshot(&report.snapshot);
    if original.is_empty() && !report.source_acquisitions.is_empty() {
        original.push(HistoricalSourceOverhead::Unavailable {
            reason: culprit_core::t!("view.source_overhead.invalid_saved"),
        });
    }
    if !original.is_empty() {
        response["source_acquisitions"] = json!(original.iter().map(value).collect::<Vec<_>>());
    }
}

#[cfg(test)]
mod tests;
