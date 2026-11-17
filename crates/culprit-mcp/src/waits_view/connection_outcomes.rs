//! The Engine base facade rewords current retained facts; MCP bounds output and never opens a source/archive.
use culprit_core::{Report, ResetMarker, telemetry::digest};
use serde::Serialize;
use serde_json::{Value, json};

const ENTRY_LIMIT: usize = 8;

pub(super) fn attach(response: &mut Value, report: &Report) {
    match culprit_engine::explain_connection_outcomes(&report.snapshot) {
        Ok(None) => {
            if let Some(reason) = report.snapshot.source_problem(&[culprit_core::source_id::SESSIONS]) {
                response["connection_outcomes_unavailable"] = json!(reason);
            }
        }
        Err(_) => {
            response["connection_outcomes_unavailable"] = json!(culprit_core::t!("view.connection_outcomes.invalid"))
        }
        Ok(Some(explained)) => {
            let (from, until) = explained.context.bounds();
            response["connection_outcomes"] = json!({
                "title":explained.title,"summary":explained.summary,"sources":explained.sources,
                "read_complete":explained.read_complete,"native_rows":explained.native_rows,"observed_rows":explained.observed_rows,
                "mapped_rows":explained.mapped_rows,"entries_retained":explained.entries.len(),
                "entries_presented":explained.entries.len().min(ENTRY_LIMIT),
                "entries_limited":explained.entries_limited || explained.entries.len()>ENTRY_LIMIT,
                "read_context":{"native":explained.context.native().is_some(),"observed_from":from,"observed_until":until},
                "entries":explained.entries.iter().take(ENTRY_LIMIT).map(|entry|json!({
                    "reference":reference("connection-outcome-point-v1",&(&report.snapshot.id,&entry.reference.source,&entry.reference.observation_key)),
                    "scope":entry.scope,"title":entry.title,"explanation":entry.explanation,
                    "metrics":entry.metrics.iter().map(|metric|json!({
                        "kind":metric.kind,"native_key":metric.native_key,"label":metric.label,"availability":metric.availability,
                        "value":metric.value,"display_value":metric.display_value,"unit":metric.unit,"nature":metric.nature,
                        "reset":reset(&metric.reset),"rate":metric.rate,"explanation":metric.explanation,
                    })).collect::<Vec<_>>(),
                    "investigation":entry.investigation,"verify":entry.verify,
                })).collect::<Vec<_>>(),
                "limitations":explained.limitations,
            });
        }
    }
}

fn reset(reset: &ResetMarker) -> Value {
    match reset {
        ResetMarker::Unknown => json!({"kind":"unknown"}),
        ResetMarker::At(at) => json!({"kind":"at","value":at}),
        ResetMarker::Generation(generation) => {
            json!({"kind":"generation","reference":reference("connection-outcome-reset-v1",generation)})
        }
    }
}
fn reference(domain: &str, value: &impl Serialize) -> Option<String> {
    serde_json::to_vec(&(domain, value)).ok().map(|bytes| digest(&bytes))
}

#[cfg(test)]
mod tests;
