//! Compact interpretation of the verified original; no native provider or raw proof is exposed.
use culprit_core::{Result, telemetry::DeliveryProblem};
use culprit_engine::LocalHyperscaleLogComponents;
use serde_json::{Value, json};

const ENTRY_LIMIT: usize = 5;

pub(super) fn captured(analysis: &LocalHyperscaleLogComponents) -> Result<Value> {
    let view = analysis.report();
    let entries = view
        .entries
        .iter()
        .take(ENTRY_LIMIT)
        .map(|entry| {
            let reference = culprit_core::telemetry::digest(
                &serde_json::to_vec(&(
                    "local-hyperscale-log-component",
                    &analysis.original().read().origin.capture,
                    &entry.observation_key,
                ))
                .map_err(|_| DeliveryProblem::Integrity.error())?,
            );
            Ok(json!({
                "reference":reference, "title":entry.title, "explanation":entry.explanation,
                "component_id":entry.component_id, "replica_id":entry.replica_id, "file_id":entry.file_id,
                "role":entry.role, "role_desc":entry.role_desc,
                "last_reported_time_without_offset":entry.last_reported_time_without_offset,
                "current_max_log_rate_bps":entry.current_max_log_rate_bps,
                "catchup_rate_bps":entry.catchup_rate_bps, "catchup_distance_bytes":entry.catchup_distance_bytes,
                "metrics":entry.metrics,
            }))
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(json!({
        "hyperscale_log_components": {
            "title":view.title, "summary":view.summary,
            "source_state":view.source_state, "source_reason":view.source_reason,
            "native_source_state":view.native_source.state, "native_rows":view.native_rows,
            "read_complete":view.read_complete, "observed_rows":view.observed_rows,
            "backend_presented_rows":view.presented_rows, "entries_presented":entries.len(),
            "entries_limited":view.entries_limited || view.entries.len()>ENTRY_LIMIT,
            "original_read_bounds":{"from":view.context.observed_from,"until":view.context.observed_until},
            "historical_context":true, "limitations":view.limitations, "entries":entries,
        },
        "collected_from":analysis.original().collected_from(), "collected_until":analysis.original().collected_until(),
        "local_verification_bytes":analysis.original().bytes_read(),
    }))
}
