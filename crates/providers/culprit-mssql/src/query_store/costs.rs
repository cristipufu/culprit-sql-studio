//! Native Query Store cost projections. Compilation totals retain their own lifetime and never borrow a runtime window.
use crate::{
    read::{integer, invalid_data, signed_integer},
    value::Rec,
};
use culprit_core::{Result, telemetry::canonical_time};

pub(crate) const LAYOUT: &str = r#"SELECT /* culprit */
 CASE WHEN EXISTS(SELECT 1 FROM sys.all_columns WHERE object_id=OBJECT_ID(N'sys.query_store_runtime_stats') AND name=N'avg_log_bytes_used') THEN 1 ELSE 0 END AS log_bytes"#;

pub(crate) fn runs_sql(with_log: bool, selected: bool) -> String {
    let selector = if selected { "DECLARE @ids xml=CONVERT(xml,@P2);" } else { "" };
    let selection = if selected {
        "AND p.query_id IN(SELECT n.x.value('@id[1]','bigint') FROM @ids.nodes('/ids/id') n(x))"
    } else {
        ""
    };
    let log = if with_log { "SUM(rs.avg_log_bytes_used*rs.count_executions)" } else { "CAST(NULL AS float)" };
    let grouping = if selected { "p.query_id," } else { "" };
    let group_by = if selected { "GROUP BY p.query_id" } else { "" };
    format!(
        r#"{selector}
{relation}
SELECT /* culprit */ {grouping}
 SUM(CASE WHEN rs.execution_type=3 THEN rs.count_executions ELSE 0 END) AS aborted,
 SUM(CASE WHEN rs.execution_type=3 THEN rs.culprit_duration_us ELSE 0 END) AS aborted_us,
 SUM(CASE WHEN rs.execution_type=4 THEN rs.count_executions ELSE 0 END) AS failed,
 {log} AS log_bytes
FROM culprit_runtime rs
JOIN sys.query_store_runtime_stats_interval i ON i.runtime_stats_interval_id=rs.runtime_stats_interval_id
JOIN sys.query_store_plan p ON p.plan_id=rs.plan_id
JOIN sys.query_store_query q ON q.query_id=p.query_id
LEFT JOIN sys.query_store_query_text t ON t.query_text_id=q.query_text_id
WHERE i.end_time>=DATEADD(minute,-@P1,SYSDATETIMEOFFSET()) AND {workload} {selection}
{group_by} OPTION(MAXDOP 1)"#,
        relation = super::runtime::relation(false),
        workload = crate::statement_origin::application_candidates_sql("t.query_sql_text")
    )
}

pub(crate) struct RunCosts {
    pub query_id: String,
    pub aborted: u64,
    pub aborted_us: f64,
    pub failed: u64,
    pub log_bytes: Option<f64>,
}
impl RunCosts {
    pub fn decode(row: &Rec) -> Result<Self> {
        Ok(Self {
            query_id: integer(row, "query_id")?.to_string(),
            aborted: integer(row, "aborted")?.try_into().map_err(|_| invalid_data())?,
            aborted_us: crate::read::required_nonnegative_number(row, "aborted_us")?,
            failed: integer(row, "failed")?.try_into().map_err(|_| invalid_data())?,
            log_bytes: crate::read::optional_nonnegative_number(row, "log_bytes")?,
        })
    }
}

pub(crate) const COMPILATION_COLUMNS: &str = r#"q.query_id,q.count_compiles,q.avg_compile_duration,
 CONVERT(varchar(40),q.initial_compile_start_time,127) AS first_compile,
 CONVERT(varchar(40),q.last_compile_start_time,127) AS last_compile,
 DATEDIFF_BIG(second,q.initial_compile_start_time,SYSDATETIMEOFFSET()) AS compiling_s"#;

pub(crate) fn compilation_sql() -> String {
    format!(
        r#"DECLARE /* culprit */ @ids xml=CONVERT(xml,@P1);
SELECT /* culprit */ {COMPILATION_COLUMNS}
FROM sys.query_store_query q
WHERE q.query_id IN(SELECT n.x.value('@id[1]','bigint') FROM @ids.nodes('/ids/id') n(x))
OPTION(MAXDOP 1)"#
    )
}

/// Bounded export of retained entries, including Culprit's own compilation overhead.
pub(crate) fn compilation_snapshot_sql() -> String {
    format!(
        r#"SELECT /* culprit */ TOP (@P1) {COMPILATION_COLUMNS}
FROM sys.query_store_query q
OPTION(MAXDOP 1)"#
    )
}

pub(crate) struct Compilation {
    pub query_id: String,
    pub count: u64,
    pub avg_duration_us: f64,
    pub first_at: String,
    pub last_at: String,
    pub seconds_since_first: i64,
}
impl Compilation {
    pub fn decode(row: &Rec) -> Result<Self> {
        Ok(Self {
            query_id: integer(row, "query_id")?.to_string(),
            count: integer(row, "count_compiles")?.try_into().map_err(|_| invalid_data())?,
            avg_duration_us: crate::read::required_nonnegative_number(row, "avg_compile_duration")?,
            first_at: canonical_time(&crate::read::text(row, "first_compile")?)?,
            last_at: canonical_time(&crate::read::text(row, "last_compile")?)?,
            seconds_since_first: signed_integer(row, "compiling_s")?,
        })
    }
    pub fn observation(&self) -> culprit_core::telemetry::MssqlQueryCompilation {
        culprit_core::telemetry::MssqlQueryCompilation {
            query_id: self.query_id.clone(),
            first_compile_at: self.first_at.clone(),
            last_compile_at: self.last_at.clone(),
            compilations: self.count.to_string(),
            average_duration_us: self.avg_duration_us,
            seconds_since_first_compile: self.seconds_since_first,
            reset: culprit_core::ResetMarker::Unknown,
        }
    }
    pub fn within_window(&self, minutes: u32) -> bool {
        self.count > 0 && self.seconds_since_first >= 0 && self.seconds_since_first <= i64::from(minutes) * 60
    }
    pub fn total_ms(&self) -> f64 {
        super::runtime::milliseconds(self.avg_duration_us * self.count as f64)
    }
}

pub(crate) fn database_compilation_sql() -> String {
    format!(
        r#"SELECT /* culprit */ SUM(CAST(q.count_compiles AS float)) AS compiles,
 SUM(q.avg_compile_duration*q.count_compiles)/1000.0 AS compile_ms,
 CONVERT(varchar(40),MIN(q.initial_compile_start_time),127) AS first_compile,
 CONVERT(varchar(40),MAX(q.last_compile_start_time),127) AS last_compile
FROM sys.query_store_query q
LEFT JOIN sys.query_store_query_text t ON t.query_text_id=q.query_text_id
WHERE q.is_internal_query=0 AND q.is_clouddb_internal_query=0 AND {workload} OPTION(MAXDOP 1)"#,
        workload = crate::statement_origin::application_candidates_sql("t.query_sql_text")
    )
}
