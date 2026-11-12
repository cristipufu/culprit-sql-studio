//! Native query-family projection. Cache and Query Store use one parameter-declaration grammar.
/// A statement's text without the parameter declarations a parameterized one starts with
/// (`(@p0 nvarchar(7),@p1 int)SELECT …` → `SELECT …`): the declarations end at the first `)` followed by neither `,`
/// nor another `)`. `{text}` is the T-SQL expression of the text. Hashed on its first 4,000 characters (HASHBYTES takes
/// 8,000 bytes before SQL Server 2016).
pub(crate) fn body_sql(text: &str) -> String {
    format!(
        "CASE WHEN {text} LIKE N'(@%' AND PATINDEX(N'%)[^,)]%', {text}) > 0
              THEN SUBSTRING({text}, PATINDEX(N'%)[^,)]%', {text}) + 1, 2147483647) ELSE {text} END"
    )
}

/// Query Store queries grouped by query_hash: a family with many members is the same statement sent with different
/// literals. Members are ranked by time so the family links to its heaviest variants. A variant is a text that differs
/// once its parameter declarations are set aside: the same statement sent with `nvarchar(7)` and `nvarchar(9)`
/// parameters (ADO.NET's AddWithValue sizes them to each value) or written into several procedures shares a
/// query_hash, but has no literal baked in. Culprit's own statements are left out.
pub(crate) fn sql() -> String {
    format!(
        r#"
{relation}, per_query AS (
  SELECT q.query_hash, q.query_id, q.query_text_id,
    SUM(rs.count_executions) AS executions,
    SUM(rs.culprit_duration_us) / 1000.0 AS total_ms
  FROM sys.query_store_query q
  JOIN sys.query_store_plan p ON p.query_id = q.query_id
  JOIN culprit_runtime rs ON rs.plan_id = p.plan_id
  JOIN sys.query_store_runtime_stats_interval i ON i.runtime_stats_interval_id = rs.runtime_stats_interval_id
  WHERE i.end_time > DATEADD(minute, -@P1, SYSDATETIMEOFFSET())
  GROUP BY q.query_hash, q.query_id, q.query_text_id
),
members AS (
  SELECT pq.query_hash, pq.query_id, pq.query_text_id, pq.executions, pq.total_ms,
    HASHBYTES('SHA2_256', LEFT({body}, 4000)) AS body_hash
  FROM per_query pq
  JOIN sys.query_store_query_text qt ON qt.query_text_id = pq.query_text_id
  WHERE {application}
),
families AS (
  SELECT TOP 15 query_hash, COUNT(DISTINCT body_hash) AS variants, SUM(executions) AS executions, SUM(total_ms) AS total_ms
  FROM members
  GROUP BY query_hash
  HAVING COUNT(DISTINCT body_hash) >= 10
  ORDER BY COUNT(DISTINCT body_hash) DESC
),
ranked AS (
  SELECT m.query_hash, m.query_id, m.query_text_id,
    ROW_NUMBER() OVER (PARTITION BY m.query_hash ORDER BY m.total_ms DESC) AS rn
  FROM members m JOIN families f ON f.query_hash = m.query_hash
)
SELECT CONVERT(varchar(20), f.query_hash, 1) AS query_hash, f.variants, f.executions, f.total_ms,
  CAST(r.query_id AS varchar(20)) AS query_id, r.rn,
  CASE WHEN r.rn = 1 THEN LEFT(qt.query_sql_text, 600) END AS sample
FROM families f
JOIN ranked r ON r.query_hash = f.query_hash AND r.rn <= 5
JOIN sys.query_store_query_text qt ON qt.query_text_id = r.query_text_id
ORDER BY f.variants DESC, r.rn OPTION(MAXDOP 1)
"#,
        body = body_sql("qt.query_sql_text"),
        application = crate::statement_origin::application_candidates_sql("qt.query_sql_text"),
        relation = super::runtime::relation(false).trim_end(),
    )
}
