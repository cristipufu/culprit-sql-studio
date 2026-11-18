//! Compact informational values do not disclose private native labels or make per-database pressure claims.
use culprit_core::{PerformanceCounterReport, Report};
use serde_json::{Value, json};
const ENTRY_LIMIT: usize = 5;

pub(super) fn attach(response: &mut Value, report: &Report) {
    if let Some(counters) = &report.performance_counters {
        response["performance_counters"] = value(counters);
    } else if report
        .snapshot
        .sources
        .iter()
        .any(|source| source.source == culprit_core::source_id::PERFORMANCE_COUNTERS)
    {
        response["performance_counters_unavailable"] = json!(culprit_core::t!("view.performance_counters.invalid"));
    }
}

pub(crate) fn value(counters: &PerformanceCounterReport) -> Value {
    json!({
        "title":counters.title,"summary":counters.summary,"source_state":counters.source_state,
        "read_complete":counters.read_complete,"native_observations":counters.native_observations,"observed_observations":counters.observed_observations,
        "native_counter_rows":counters.native_counter_rows,"observed_counter_rows":counters.observed_counter_rows,
        "presented_counter_rows":counters.presented_counter_rows,"returned_counter_rows":counters.entries.len().min(ENTRY_LIMIT),"omitted_counter_rows":counters.entries.len().saturating_sub(ENTRY_LIMIT),"entries_limited":counters.entries_limited || counters.entries.len()>ENTRY_LIMIT,
        "fields_withheld":counters.fields_withheld,"missing_fields":counters.missing_fields,
        "entries":counters.entries.iter().take(ENTRY_LIMIT).map(|entry|json!({
            "reference":culprit_core::telemetry::digest(format!("{}:{}",entry.observation_key,entry.counter_key).as_bytes()),
            "field":entry.field,"family":entry.family,"scope":entry.scope,"availability":entry.availability,
            "observed_at":entry.observed_at,"nature":entry.nature,"unit":entry.unit,"reset":entry.reset,
            "metric":entry.metric,"explanation":entry.explanation,
        })).collect::<Vec<_>>(),"limitations":counters.limitations,
    })
}

#[cfg(test)]
mod tests;
