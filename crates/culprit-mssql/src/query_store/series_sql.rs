//! Native source operations for database-side Query Store bucket aggregation.
/// Bucket number of an interval's start: whole buckets since 1970-01-01 UTC.
fn bucket_expr(minutes: u32) -> String {
    format!("DATEDIFF(minute, '19700101', CAST(SWITCHOFFSET(i.start_time, '+00:00') AS datetime2(0))) / {minutes}")
}

pub(crate) fn totals_sql(window_minutes: u32, minutes: u32) -> String {
    let bucket = bucket_expr(minutes);
    format!(
        r#"
DECLARE @since datetimeoffset = DATEADD(minute, -{window_minutes}, SYSDATETIMEOFFSET());
{relation}
SELECT {bucket} AS bucket,
  SUM(rs.count_executions) AS executions,
  SUM(rs.culprit_duration_us) / 1000.0 AS total_ms
FROM culprit_runtime rs
JOIN sys.query_store_runtime_stats_interval i ON i.runtime_stats_interval_id = rs.runtime_stats_interval_id
JOIN sys.query_store_plan p ON p.plan_id = rs.plan_id
JOIN sys.query_store_query q ON q.query_id = p.query_id
JOIN sys.query_store_query_text t ON t.query_text_id = q.query_text_id
WHERE i.end_time >= @since AND {workload}
GROUP BY {bucket}
"#,
        relation = super::runtime::relation(false),
        workload = crate::statement_origin::application_candidates_sql("t.query_sql_text"),
    )
}

pub(crate) fn per_query_sql(window_minutes: u32, minutes: u32, ids: &str) -> String {
    let bucket = bucket_expr(minutes);
    format!(
        r#"
DECLARE @since datetimeoffset = DATEADD(minute, -{window_minutes}, SYSDATETIMEOFFSET());
{relation}
SELECT p.query_id, {bucket} AS bucket,
  SUM(rs.count_executions) AS executions,
  SUM(rs.culprit_duration_us) / 1000.0 AS total_ms
FROM culprit_runtime rs
JOIN sys.query_store_runtime_stats_interval i ON i.runtime_stats_interval_id = rs.runtime_stats_interval_id
JOIN sys.query_store_plan p ON p.plan_id = rs.plan_id
WHERE i.end_time >= @since AND p.query_id IN ({ids})
GROUP BY p.query_id, {bucket}
"#,
        relation = super::runtime::relation(false)
    )
}

pub(crate) const INTERVAL_SQL: &str =
    "SELECT CAST(interval_length_minutes AS int) AS minutes FROM sys.database_query_store_options";
