//! MCP projects the shared job explanation; it does not reacquire or reclassify native scheduler records.
use culprit_core::{
    JobEntry, Metric, Report,
    telemetry::{MSSQL_AGENT_SOURCES, MYSQL_SCHEDULED_EVENTS_SOURCE, TIMESCALE_JOB_RUNS_SOURCE, TIMESCALE_JOBS_SOURCE},
};
use serde_json::{Value, json};
const ENTRY_LIMIT: usize = 5;
pub(super) fn attach(response: &mut Value, report: &Report) {
    let Ok(Some(view)) = culprit_engine::explain_jobs(&report.snapshot) else {
        let sources = report
            .snapshot
            .sources
            .iter()
            .filter(|source| {
                matches!(
                    source.source.as_str(),
                    TIMESCALE_JOBS_SOURCE | TIMESCALE_JOB_RUNS_SOURCE | MYSQL_SCHEDULED_EVENTS_SOURCE
                ) || MSSQL_AGENT_SOURCES.contains(&source.source.as_str())
            })
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
    json!({"reference":{"source":entry.source,"observation_key":entry.observation_key},"job_id":entry.job_id,"kind":entry.kind,"title":entry.title,"explanation":entry.explanation,
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
    const MYSQL_KEYS: &[&str] = &[
        "view.mysql_events.field.name",
        "view.mysql_events.field.schema",
        "view.mysql_events.field.observed_at",
        "view.mysql_events.field.type",
        "view.mysql_events.field.time_zone",
        "view.mysql_events.field.interval_value",
        "view.mysql_events.field.interval_field",
        "view.mysql_events.field.execute_at",
        "view.mysql_events.field.starts",
        "view.mysql_events.field.ends",
        "view.mysql_events.field.last_started",
        "view.mysql_events.field.created",
        "view.mysql_events.field.altered",
    ];
    const AGENT_KEYS: &[&str] = &[
        "view.mssql_agent_jobs.field.name",
        "view.mssql_agent_jobs.field.stored_step",
        "view.mssql_agent_jobs.field.subsystem",
        "view.mssql_agent_jobs.field.database",
        "view.mssql_agent_jobs.field.service_name",
        "view.mssql_agent_jobs.field.observed_at",
        "view.mssql_agent_jobs.field.enabled",
        "view.mssql_agent_jobs.field.version",
        "view.mssql_agent_jobs.field.start_step",
        "view.mssql_agent_jobs.field.created",
        "view.mssql_agent_jobs.field.modified",
        "view.mssql_agent_jobs.field.requested",
        "view.mssql_agent_jobs.field.queued",
        "view.mssql_agent_jobs.field.started",
        "view.mssql_agent_jobs.field.stopped",
        "view.mssql_agent_jobs.field.last_step",
        "view.mssql_agent_jobs.field.last_step_time",
        "view.mssql_agent_jobs.field.next_scheduled",
        "view.mssql_agent_jobs.field.step_id",
        "view.mssql_agent_jobs.field.outcome",
        "view.mssql_agent_jobs.field.run_date",
        "view.mssql_agent_jobs.field.run_time",
        "view.mssql_agent_jobs.field.duration",
        "view.mssql_agent_jobs.field.sql_message",
        "view.mssql_agent_jobs.field.sql_severity",
        "view.mssql_agent_jobs.field.retries",
        "view.mssql_agent_jobs.field.bound",
        "view.mssql_agent_jobs.field.agent_start",
        "view.mssql_agent_jobs.field.service_status",
        "view.mssql_agent_jobs.field.service_start",
        "view.mssql_agent_jobs.field.service_limit",
    ];
    KEYS.contains(&metric.key.as_str())
        || AGENT_KEYS.contains(&metric.key.as_str())
        || MYSQL_KEYS.contains(&metric.key.as_str())
}
#[cfg(test)]
mod tests;
