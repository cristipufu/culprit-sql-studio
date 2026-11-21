//! Public row links come only from the authenticated original WaitCounterSamples stream.
use culprit_core::{Result, telemetry::DeliveryProblem};
use culprit_engine::LocalWaitCounters;
use serde_json::{Value, json};
pub(super) fn captured(analysis: &LocalWaitCounters) -> Result<Value> {
    let mut report = crate::waits_view::wait_counters::value(analysis.report());
    for entry in report["entries"].as_array_mut().into_iter().flatten() {
        let source = entry["source"].as_str().ok_or_else(|| DeliveryProblem::Integrity.error())?;
        let wait_type = entry["wait_type"].as_str().ok_or_else(|| DeliveryProblem::Integrity.error())?;
        let reference = analysis.reference(source, wait_type)?.key()?;
        entry["reference"] = json!(reference);
    }
    let original = analysis.report();
    let bounds = original.context.as_ref().map(|context| {
        let (from, until) = context.bounds();
        json!({"from":from,"until":until})
    });
    Ok(
        json!({"native_rows":original.native_rows,"observed_rows":original.observed_rows,"read_complete":original.read_complete,"native_sources":original.sources,"original_read_bounds":bounds,"analysis":"wait_counters","historical_context":true,"wait_counters":report,
        "collected_from":analysis.collected_from(),"collected_until":analysis.collected_until(),"local_verification_bytes":analysis.bytes_read()}),
    )
}
