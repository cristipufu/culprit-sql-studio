//! MCP projects the shared job explanation; it does not reacquire or reclassify native scheduler records.
use culprit_core::{
    JobEntry, Metric, Report,
    telemetry::{TIMESCALE_JOB_RUNS_SOURCE, TIMESCALE_JOBS_SOURCE},
};
use serde_json::{Value, json};
const ENTRY_LIMIT: usize = 5;
pub(super) fn attach(response: &mut Value, report: &Report) {
    let Ok(Some(view)) = culprit_engine::explain_jobs(&report.snapshot) else {
        let sources = report
            .snapshot
            .sources
            .iter()
            .filter(|source| matches!(source.source.as_str(), TIMESCALE_JOBS_SOURCE | TIMESCALE_JOB_RUNS_SOURCE))
            .collect::<Vec<_>>();
        if report.snapshot.operations.jobs.is_some() || !sources.is_empty() {
            let reason = if report.snapshot.operations.jobs.is_some() {
                culprit_core::t!("view.jobs.invalid")
            } else {
                culprit_core::t!("view.jobs.source.unavailable")
            };
            response["jobs_unavailable"] = json!({ "reason": reason, "sources": sources });
        }
        return;
    };
    response["jobs"] = json!({"title":view.title,"summary":view.summary,"sources":view.sources,"read_complete":view.read_complete,
        "read_bounds":{"observed_from":view.context.observed_from,"observed_until":view.context.observed_until},
        "native_rows":view.native_rows,"observed_rows":view.observed_rows,"entries_retained":view.entries.len(),"entries_presented":view.entries.len().min(ENTRY_LIMIT),"entries_limited":view.entries_limited||view.entries.len()>ENTRY_LIMIT,
        "entries":view.entries.iter().take(ENTRY_LIMIT).map(point).collect::<Vec<_>>(),"limitations":view.limitations});
}
fn point(entry: &JobEntry) -> Value {
    json!({"reference":{"source":entry.source,"observation_key":entry.observation_key},"kind":entry.kind,"title":entry.title,"explanation":entry.explanation,
        "metrics":entry.metrics.iter().filter(|metric|safe_metric(metric)).collect::<Vec<_>>(),"investigation":entry.investigation,"verify":entry.verify})
}
fn safe_metric(metric: &Metric) -> bool {
    const KEYS: &[&str] = &[
        "view.jobs.metric.schedule_interval",
        "view.jobs.metric.max_runtime",
        "view.jobs.metric.max_retries",
        "view.jobs.metric.retry_period",
        "view.jobs.metric.initial_start",
        "view.jobs.metric.next_start",
        "view.jobs.metric.last_status",
        "view.jobs.metric.total_runs",
        "view.jobs.metric.total_successes",
        "view.jobs.metric.total_failures",
        "view.jobs.metric.last_duration",
        "view.jobs.metric.last_start",
        "view.jobs.metric.last_success",
        "view.jobs.metric.stats_next_start",
        "view.jobs.metric.started_at",
        "view.jobs.metric.finished_at",
        "view.jobs.metric.sql_state",
        "view.jobs.metric.observed_at",
        "view.jobs.metric.scheduled",
        "view.jobs.metric.fixed_schedule",
        "view.jobs.metric.succeeded",
    ];
    KEYS.contains(&metric.key.as_str())
}
#[cfg(test)]
mod tests;
