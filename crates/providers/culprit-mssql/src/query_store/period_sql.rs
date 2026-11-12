//! Native period selection and aggregates; query text uses the common bounded definition reader.
use crate::sql_time::utc_offset_iso;
const MAX_QUERIES: usize = 400;
/// A timestamp as a `datetimeoffset` literal. Only ever built from a parsed timestamp, so nothing from outside
/// reaches the SQL text.
pub(crate) fn literal(iso: &str) -> Option<String> {
    let t = chrono::DateTime::parse_from_rfc3339(iso).ok()?.with_timezone(&chrono::Utc);
    Some(format!("'{}'", t.format("%Y-%m-%dT%H:%M:%S+00:00")))
}

/// Period boundaries snap to the start of the Query Store interval they fall in.
fn snapped(b0: &str, r0: &str) -> String {
    format!(
        r#"
DECLARE @b0 datetimeoffset = {b0}, @r0 datetimeoffset = {r0};
SELECT @b0 = ISNULL((SELECT MAX(start_time) FROM sys.query_store_runtime_stats_interval WHERE start_time <= @b0 AND end_time > @b0), @b0);
SELECT @r0 = ISNULL((SELECT MAX(start_time) FROM sys.query_store_runtime_stats_interval WHERE start_time <= @r0 AND end_time > @r0), @r0);"#
    )
}

fn intervals_cte(b0: &str, r0: &str, r1: &str) -> String {
    format!(
        r#"{snap}
DECLARE @r1 datetimeoffset = {r1};
{relation}, s AS (
  SELECT p.query_id, rs.plan_id, CASE WHEN i.start_time >= @r0 THEN 1 ELSE 0 END AS recent,
         rs.count_executions AS n, rs.culprit_duration_us AS total_us, rs.min_duration AS min_us, rs.max_duration AS max_us,
         i.start_time, i.end_time
  FROM culprit_runtime rs
  JOIN sys.query_store_runtime_stats_interval i ON i.runtime_stats_interval_id = rs.runtime_stats_interval_id
  JOIN sys.query_store_plan p ON p.plan_id = rs.plan_id
  JOIN sys.query_store_query q ON q.query_id = p.query_id
  JOIN sys.query_store_query_text t ON t.query_text_id = q.query_text_id
  WHERE i.start_time >= @b0 AND i.start_time < @r1 AND {workload}
)"#,
        snap = snapped(b0, r0),
        relation = crate::query_store::runtime::relation(false).trim_end(),
        workload = crate::statement_origin::application_candidates_sql("t.query_sql_text"),
    )
}

pub(crate) fn per_plan_sql(b0: &str, r0: &str, r1: &str) -> String {
    format!(
        r#"{cte},
agg AS (
  SELECT query_id, plan_id, recent, SUM(n) AS executions, SUM(total_us) / 1000.0 AS total_ms,
         MIN(min_us) / 1000.0 AS min_ms, MAX(max_us) / 1000.0 AS max_ms, MIN(start_time) AS first_start
  FROM s GROUP BY query_id, plan_id, recent
),
top_q AS (SELECT TOP ({MAX_QUERIES}) query_id FROM agg GROUP BY query_id ORDER BY SUM(total_ms) DESC)
SELECT a.query_id, a.plan_id, a.recent, a.executions, a.total_ms, a.min_ms, a.max_ms, {first} AS first_start
FROM agg a JOIN top_q t ON t.query_id = a.query_id
"#,
        cte = intervals_cte(b0, r0, r1),
        first = utc_offset_iso("a.first_start"),
    )
}

pub(crate) fn totals_sql(b0: &str, r0: &str, r1: &str) -> String {
    format!(
        r#"{cte}
SELECT recent, SUM(n) AS executions, SUM(total_us) / 1000.0 AS total_ms,
       {start} AS period_start, {end} AS period_end
FROM s GROUP BY recent
"#,
        cte = intervals_cte(b0, r0, r1),
        start = utc_offset_iso("MIN(start_time)"),
        end = utc_offset_iso("MAX(end_time)"),
    )
}

/// Seconds of the (snapped) baseline period that have at least one Query Store interval.
pub(crate) fn coverage_sql(b0: &str, r0: &str) -> String {
    format!(
        r#"{snap}
SELECT SUM(CAST(DATEDIFF(second, start_time, CASE WHEN end_time > @r0 THEN @r0 ELSE end_time END) AS float)) AS covered,
       CAST(DATEDIFF(second, @b0, @r0) AS float) AS wanted
FROM sys.query_store_runtime_stats_interval
WHERE start_time >= @b0 AND start_time < @r0
"#,
        snap = snapped(b0, r0),
    )
}
