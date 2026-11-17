//! A compact public view keeps native counts but never exposes the raw source body or physical cache labels.
use culprit_core::Report;
use serde_json::{Value, json};
const ENTRY_LIMIT: usize = 16;

pub(super) fn attach(response: &mut Value, report: &Report) {
    if let Some(ratios) = &report.counter_ratios {
        response["counter_ratios"] = json!({
            "title":ratios.title,"summary":ratios.summary,"source_state":ratios.source_state,
            "read_complete":ratios.read_complete,"declared_ratios":ratios.declared_ratios,
            "presented_ratios":ratios.entries.len().min(ENTRY_LIMIT),"entries_limited":ratios.entries_limited || ratios.entries.len()>ENTRY_LIMIT,
            "entries":ratios.entries.iter().take(ENTRY_LIMIT).map(|entry|json!({
                "reference":culprit_core::telemetry::digest(entry.observation_key.as_bytes()),"kind":entry.kind,"scope":entry.scope,
                "observed_at":entry.observed_at,"title":entry.title,"explanation":entry.explanation,
                "issues":entry.issues,"metrics":entry.metrics,
            })).collect::<Vec<_>>(),"limitations":ratios.limitations,
        });
    } else if report
        .snapshot
        .pressure
        .as_ref()
        .and_then(|pressure| pressure.resource_counters.as_ref())
        .is_some_and(|samples| samples.iter().any(|sample| !sample.ratios.is_empty()))
        || report.snapshot.source_problem(&[culprit_core::source_id::PERFORMANCE_COUNTERS]).is_some()
    {
        response["counter_ratios_unavailable"] = json!(culprit_core::t!("view.counter_ratios.invalid"));
    }
}

#[cfg(test)]
mod tests;
