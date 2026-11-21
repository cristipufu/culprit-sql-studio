//! Compact original topology/state interpretation, with authenticated references and no raw source proof.
use culprit_core::{Result, telemetry::DeliveryProblem};
use culprit_engine::LocalReplication;
use serde_json::{Value, json};
const ENTRY_LIMIT: usize = 5;
pub(super) fn captured(analysis: &LocalReplication) -> Result<Value> {
    let report = analysis.report();
    let context = report.context.native().ok_or_else(|| DeliveryProblem::Integrity.error())?;
    let entries = report
        .entries
        .iter()
        .take(ENTRY_LIMIT)
        .map(|entry| {
            let reference = analysis.reference(&entry.reference)?.key()?;
            Ok(json!({"reference":reference,"scope":entry.scope,"conditions":entry.conditions,
            "title":entry.title,"explanation":entry.explanation,"fields":entry.fields,"groups":entry.groups,
            "investigation":entry.investigation,"verify":entry.verify}))
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(json!({
        "analysis":"replication","historical_context":true,"native_scope_qualified":context.has_qualified_local_owner(),
        "original_read_bounds":{"from":context.observed_from,"until":context.observed_until},
        "native_rows":report.native_rows,"observed_rows":report.observed_rows,"read_complete":report.read_complete,
        "native_sources":report.sources,"collected_from":analysis.collected_from(),"collected_until":analysis.collected_until(),
        "local_verification_bytes":analysis.bytes_read(),
        "replication":{"title":report.title,"summary":report.summary,"sources":report.sources,"read_complete":report.read_complete,
            "native_rows":report.native_rows,"observed_rows":report.observed_rows,"state_rows":report.state_rows,
            "entries_presented":entries.len(),"entries_limited":report.entries_limited || report.entries.len()>ENTRY_LIMIT,
            "entries":entries,"limitations":report.limitations}
    }))
}
