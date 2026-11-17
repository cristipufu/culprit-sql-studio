use super::*;
fn optional_text(row: &Row, column: &str) -> Result<Option<String>> {
    row.value(column).ok_or_else(invalid).map(|value| value.map(str::to_owned))
}
fn optional_flag(row: &Row, column: &str) -> Result<Option<bool>> {
    match row.value(column) {
        Some(None) => Ok(None),
        Some(Some(_)) => flag(row, column).map(Some),
        None => Err(invalid()),
    }
}
fn timestamp(row: &Row, column: &str) -> Result<Option<TimescaleJobTimestamp>> {
    Ok(optional_text(row, column)?.map(|value| match value.as_str() {
        "infinity" => TimescaleJobTimestamp::PositiveInfinity,
        "-infinity" => TimescaleJobTimestamp::NegativeInfinity,
        _ => TimescaleJobTimestamp::Finite { at: value },
    }))
}
fn procedure(row: &Row, schema: &str, name: &str) -> Result<Option<TimescaleJobProcedure>> {
    match (optional_text(row, schema)?, optional_text(row, name)?) {
        (Some(schema), Some(name)) => Ok(Some(TimescaleJobProcedure { schema, name })),
        (None, None) => Ok(None),
        _ => Err(invalid()),
    }
}
pub(super) fn state(row: &Row, position: u32) -> Result<TimescaleJobState> {
    let statistics = if flag(row, "stats_present")? {
        Some(TimescaleJobStatistics {
            reported_job_state: optional_text(row, "stats_job_status")?.map(TimescaleReportedJobState::from_native),
            last_run_status: optional_text(row, "stats_last_run_status")?.map(TimescaleJobLastRun::from_native),
            last_run_started_at: timestamp(row, "stats_last_run_started_at")?,
            last_successful_finish: timestamp(row, "stats_last_successful_finish")?,
            last_run_duration: optional_text(row, "stats_last_run_duration")?,
            next_start: timestamp(row, "stats_next_start")?,
            total_runs: row.unsigned_text("stats_total_runs")?,
            total_successes: row.unsigned_text("stats_total_successes")?,
            total_failures: row.unsigned_text("stats_total_failures")?,
        })
    } else {
        None
    };
    let point = TimescaleJobState {
        position,
        observed_at: text(row, "observed_at")?,
        job_id: text(row, "job_id")?,
        procedure: TimescaleJobProcedure { schema: text(row, "proc_schema")?, name: text(row, "proc_name")? },
        owner: text(row, "owner")?,
        scheduled: flag(row, "scheduled")?,
        schedule_interval: text(row, "schedule_interval")?,
        max_runtime: text(row, "max_runtime")?,
        max_retries: text(row, "max_retries")?,
        retry_period: text(row, "retry_period")?,
        fixed_schedule: optional_flag(row, "fixed_schedule")?,
        initial_start: timestamp(row, "initial_start")?,
        next_start: timestamp(row, "next_start")?,
        hypertable: procedure(row, "hypertable_schema", "hypertable_name")?,
        statistics,
    };
    if !point.is_valid() {
        return Err(invalid());
    }
    Ok(point)
}
pub(super) fn run(row: &Row, position: u32, origin: TimescaleJobProcedureOrigin) -> Result<TimescaleJobRun> {
    let point = TimescaleJobRun {
        position,
        observed_at: text(row, "observed_at")?,
        native_run_id: row.unsigned_text("native_run_id")?,
        job_id: text(row, "job_id")?,
        procedure: procedure(row, "proc_schema", "proc_name")?
            .map(|label| TimescaleReportedJobProcedure { label, origin }),
        process_id: row.unsigned_text("process_id")?,
        started_at: text(row, "started_at")?,
        finished_at: optional_text(row, "finished_at")?,
        succeeded: optional_flag(row, "succeeded")?,
        sql_state: optional_text(row, "sql_state")?,
    };
    if !point.is_valid() {
        return Err(invalid());
    }
    Ok(point)
}
