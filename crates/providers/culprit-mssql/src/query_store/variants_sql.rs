//! Native parameter-sensitive-plan aggregates use the shared runtime relation.
use crate::sql_time::utc_offset_iso;
pub(crate) fn variants_sql(ids: &str) -> String {
    format!(
        r#"
{window}
{relation}
SELECT v.parent_query_id, v.query_variant_query_id AS variant_id, p.plan_id,
  SUM(rs.count_executions) AS executions,
  SUM(rs.culprit_duration_us) / 1000.0 AS total_ms,
  MIN(rs.min_duration) / 1000.0 AS min_ms, MAX(rs.max_duration) / 1000.0 AS max_ms,
  {first} AS first_seen, {last} AS last_seen,
  MAX(CAST(p.is_forced_plan AS int)) AS is_forced
FROM sys.query_store_query_variant v
JOIN sys.query_store_plan p ON p.query_id = v.query_variant_query_id
JOIN culprit_runtime rs ON rs.plan_id = p.plan_id
JOIN sys.query_store_runtime_stats_interval i ON i.runtime_stats_interval_id = rs.runtime_stats_interval_id
WHERE {membership} AND v.parent_query_id IN ({ids})
GROUP BY v.parent_query_id, v.query_variant_query_id, p.plan_id OPTION(MAXDOP 1)
"#,
        window = super::window_sql::DECLARATION,
        membership = super::window_sql::MEMBERSHIP,
        relation = crate::query_store::runtime::relation(false),
        first = utc_offset_iso("MIN(rs.first_execution_time)"),
        last = utc_offset_iso("MAX(rs.last_execution_time)"),
    )
}
