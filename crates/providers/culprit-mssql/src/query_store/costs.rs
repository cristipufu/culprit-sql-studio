//! Native Query Store cost projections. Compilation totals retain their own lifetime and never borrow a runtime window.
use crate::{
    read::{integer, invalid_data, signed_integer},
    value::Rec,
};
use culprit_core::{Result, telemetry::canonical_time};

pub(crate) const LAYOUT: &str = r#"SELECT /* culprit */
 CASE WHEN EXISTS(SELECT 1 FROM sys.all_columns WHERE object_id=OBJECT_ID(N'sys.query_store_runtime_stats') AND name=N'avg_log_bytes_used') THEN 1 ELSE 0 END AS log_bytes"#;

pub(crate) fn runs_sql(with_log: bool) -> String {
    let population = format!(
        r#"FROM culprit_runtime rs
 JOIN sys.query_store_runtime_stats_interval i ON i.runtime_stats_interval_id=rs.runtime_stats_interval_id
 JOIN sys.query_store_plan p ON p.plan_id=rs.plan_id
 JOIN sys.query_store_query q ON q.query_id=p.query_id
 LEFT JOIN sys.query_store_query_text t ON t.query_text_id=q.query_text_id
 WHERE {membership} AND {workload}"#,
        membership = super::window_sql::MEMBERSHIP,
        workload = crate::statement_origin::application_candidates_sql("t.query_sql_text")
    );
    let window = super::window_sql::DECLARATION;
    let relation = super::runtime::relation(false);
    let log = if with_log { "SUM(rs.avg_log_bytes_used*rs.count_executions)" } else { "CAST(NULL AS float)" };
    let (denominator, join, total) = if with_log {
        (
            format!(",total AS(SELECT {log} AS total_log_bytes {population})"),
            "CROSS JOIN total",
            "total.total_log_bytes",
        )
    } else {
        (String::new(), "", "CAST(NULL AS float) AS total_log_bytes")
    };
    // Selected groups stay bounded by the requested query IDs. The all-workload denominator is a scalar,
    // not an all-query grouping/window spool; both aggregates use the exact same frozen interval population.
    format!(
        r#"{window}DECLARE /* culprit */ @ids xml=CONVERT(xml,@P2);
{relation},costs AS (
 SELECT p.query_id,q.context_settings_id,q.object_id,CONVERT(varchar(16),q.query_hash,2) AS query_hash,
 CONVERT(varchar(40),q.initial_compile_start_time,127) AS query_initial_compile_at,
 SUM(rs.count_executions) AS executions,
 SUM(CASE WHEN rs.execution_type=3 THEN rs.count_executions ELSE 0 END) AS aborted,
 SUM(CASE WHEN rs.execution_type=3 THEN rs.culprit_duration_us ELSE 0 END) AS aborted_us,
 SUM(CASE WHEN rs.execution_type=4 THEN rs.count_executions ELSE 0 END) AS failed,
 {log} AS log_bytes
 {population} AND p.query_id IN(SELECT n.x.value('@id[1]','bigint') FROM @ids.nodes('/ids/id') n(x))
 GROUP BY p.query_id,q.context_settings_id,q.object_id,q.query_hash,q.initial_compile_start_time
){denominator}
SELECT /* culprit */ costs.*,{total} FROM costs {join} OPTION(MAXDOP 1,LOOP JOIN)"#
    )
}

pub(crate) struct RunCosts {
    pub query_id: String,
    pub identity: culprit_core::telemetry::NativeQueryIdentity,
    pub executions: u64,
    pub aborted: u64,
    pub aborted_us: f64,
    pub failed: u64,
    pub log_bytes: Option<f64>,
    pub total_log_bytes: Option<f64>,
}
impl RunCosts {
    pub fn decode(row: &Rec) -> Result<Self> {
        let value = Self {
            query_id: integer(row, "query_id")?.to_string(),
            identity: super::decode::query_identity(row)?,
            executions: integer(row, "executions")?.try_into().map_err(|_| invalid_data())?,
            aborted: integer(row, "aborted")?.try_into().map_err(|_| invalid_data())?,
            aborted_us: crate::read::required_nonnegative_number(row, "aborted_us")?,
            failed: integer(row, "failed")?.try_into().map_err(|_| invalid_data())?,
            log_bytes: crate::read::optional_nonnegative_number(row, "log_bytes")?,
            total_log_bytes: crate::read::optional_nonnegative_number(row, "total_log_bytes")?,
        };
        if value.aborted.checked_add(value.failed).is_none_or(|count| count > value.executions)
            || matches!((value.log_bytes,value.total_log_bytes),(Some(own),Some(total)) if own>total)
        {
            return Err(invalid_data());
        }
        Ok(value)
    }
}

pub(crate) const COMPILATION_COLUMNS: &str = r#"q.query_id,q.count_compiles,q.avg_compile_duration,
 q.avg_compile_memory_kb,q.last_compile_memory_kb,q.max_compile_memory_kb,
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
    pub memory: Option<culprit_core::CompilationMemory>,
    pub first_at: String,
    pub last_at: String,
    pub seconds_since_first: i64,
}
impl Compilation {
    pub fn decode(row: &Rec) -> Result<Self> {
        let memory = culprit_core::CompilationMemory {
            average_kb: crate::read::optional_nonnegative_number(row, "avg_compile_memory_kb")?,
            last_kb: crate::read::optional_nonnegative_number(row, "last_compile_memory_kb")?,
            maximum_kb: crate::read::optional_nonnegative_number(row, "max_compile_memory_kb")?,
        };
        Ok(Self {
            query_id: integer(row, "query_id")?.to_string(),
            count: integer(row, "count_compiles")?.try_into().map_err(|_| invalid_data())?,
            avg_duration_us: crate::read::required_nonnegative_number(row, "avg_compile_duration")?,
            memory: memory.is_measured().then_some(memory),
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
            memory: self.memory,
            seconds_since_first_compile: self.seconds_since_first,
            reset: culprit_core::ResetMarker::Unknown,
        }
    }
    pub fn within_window(&self, minutes: u32) -> bool {
        self.count > 0 && self.seconds_since_first >= 0 && self.seconds_since_first <= i64::from(minutes) * 60
    }
    pub fn history(&self, window_minutes: u32) -> culprit_core::signals::CompileHistory {
        culprit_core::signals::CompileHistory {
            count: self.count as f64,
            total_ms: self.total_ms(),
            first_at: Some(self.first_at.clone()),
            last_at: Some(self.last_at.clone()),
            within_window: self.within_window(window_minutes),
            memory: self.memory,
        }
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
