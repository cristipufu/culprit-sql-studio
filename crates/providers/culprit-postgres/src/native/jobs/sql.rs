use super::*;
use discovery::View;
fn field(view: &View, alias: &str, column: &str) -> String {
    if view.has(column) { format!("{alias}.{column}::text") } else { "NULL::text".into() }
}
fn timestamp(view: &View, alias: &str, column: &str) -> String {
    if !view.has(column) {
        return "NULL::text".into();
    }
    let expression = format!("{alias}.{column}");
    format!(
        "CASE WHEN {expression}='infinity'::timestamptz THEN 'infinity' WHEN {expression}='-infinity'::timestamptz THEN '-infinity' ELSE {} END",
        iso(&expression)
    )
}
pub(super) fn states(jobs: &View, statistics: Option<&View>, limit: u32) -> String {
    let mut columns = vec![format!("{} AS observed_at", iso("stamp.at"))];
    for column in [
        "job_id",
        "proc_schema",
        "proc_name",
        "owner",
        "scheduled",
        "schedule_interval",
        "max_runtime",
        "max_retries",
        "retry_period",
        "fixed_schedule",
        "check_schema",
        "check_name",
        "hypertable_schema",
        "hypertable_name",
    ] {
        columns.push(format!("{} AS {column}", field(jobs, "j", column)));
    }
    for column in ["initial_start", "next_start"] {
        columns.push(format!("{} AS {column}", timestamp(jobs, "j", column)));
    }
    columns.push(if statistics.is_some() {
        "(s.job_id IS NOT NULL)::text AS stats_present".into()
    } else {
        "false::text AS stats_present".into()
    });
    for column in
        ["job_status", "last_run_status", "last_run_duration", "total_runs", "total_successes", "total_failures"]
    {
        let value = statistics.map_or_else(|| "NULL::text".into(), |view| field(view, "s", column));
        columns.push(format!("{value} AS stats_{column}"));
    }
    for column in ["last_run_started_at", "last_successful_finish", "next_start"] {
        let value = statistics.map_or_else(|| "NULL::text".into(), |view| timestamp(view, "s", column));
        columns.push(format!("{value} AS stats_{column}"));
    }
    let join =
        if statistics.is_some() { "LEFT JOIN timescaledb_information.job_stats s ON s.job_id=j.job_id" } else { "" };
    format!(
        "WITH stamp AS (SELECT pg_catalog.clock_timestamp() AS at) SELECT {} FROM timescaledb_information.jobs j {join} CROSS JOIN stamp ORDER BY j.job_id LIMIT {limit}",
        columns.join(",")
    )
}
pub(super) fn runs(view: &View, window: &TimescaleJobRunWindow, limit: u32) -> String {
    let mut columns = vec![format!("{} AS observed_at", iso("stamp.at"))];
    columns.push(if view.capability.kind == TimescaleJobViewKind::History {
        "r.id::text AS native_run_id".into()
    } else {
        "NULL::text AS native_run_id".into()
    });
    columns.push(if view.capability.kind == TimescaleJobViewKind::History {
        "r.succeeded::text AS succeeded".into()
    } else {
        "NULL::text AS succeeded".into()
    });
    for (native, output) in [
        ("job_id", "job_id"),
        ("pid", "process_id"),
        ("sqlerrcode", "sql_state"),
        ("proc_schema", "proc_schema"),
        ("proc_name", "proc_name"),
    ] {
        columns.push(format!("{} AS {output}", field(view, "r", native)));
    }
    columns.push(format!("{} AS started_at", iso("r.start_time")));
    columns.push(format!("{} AS finished_at", iso("r.finish_time")));
    let order = if view.capability.kind == TimescaleJobViewKind::History {
        "r.start_time DESC,r.id DESC"
    } else {
        "r.start_time DESC,r.job_id"
    };
    format!(
        "WITH stamp AS (SELECT pg_catalog.clock_timestamp() AS at) SELECT {} FROM timescaledb_information.{} r CROSS JOIN stamp WHERE r.start_time>='{}'::timestamptz AND r.start_time<='{}'::timestamptz ORDER BY {order} LIMIT {limit}",
        columns.join(","),
        view.capability.kind.native_name(),
        window.from,
        window.until
    )
}
