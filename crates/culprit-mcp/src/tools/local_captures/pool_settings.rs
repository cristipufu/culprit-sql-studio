//! Exact native-name references resolve only against the authenticated original Settings read.
use culprit_core::Result;
use culprit_engine::LocalPoolSettings;
use serde_json::{Value, json};
pub(super) fn captured(analysis: &LocalPoolSettings) -> Result<Value> {
    let mut report = crate::waits_view::pool_settings::value(analysis.report());
    for field in report["fields"].as_array_mut().into_iter().flatten() {
        let name = field["key"].as_str().ok_or_else(|| culprit_core::telemetry::DeliveryProblem::Integrity.error())?;
        let reference = analysis.reference(name)?.key()?;
        field["reference"] = json!(reference);
    }
    let original = analysis.report();
    let bounds = original
        .context
        .as_ref()
        .and_then(|context| context.native())
        .map(|context| json!({"from":context.observed_from,"until":context.observed_until}));
    Ok(
        json!({"native_rows":original.native_rows,"observed_rows":original.observed_rows,"read_complete":original.read_complete,"original_read_bounds":bounds,"native_sources":original.native_source.iter().collect::<Vec<_>>(),"analysis":"pool_settings","historical_context":true,"collected_from":analysis.collected_from(),"collected_until":analysis.collected_until(),"local_verification_bytes":analysis.bytes_read(),"pool_settings":report}),
    )
}
