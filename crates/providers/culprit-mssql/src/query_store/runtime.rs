//! One native runtime relation and sufficient-statistics interpretation for raw fragments and summaries.
use crate::{
    read::{
        integer, invalid_data, optional_nonnegative_number as optional_number, required_nonnegative_number as number,
    },
    value::Rec,
};
use culprit_core::{Result, telemetry::canonical_time};

pub(crate) const MICROSECONDS_PER_MILLISECOND: f64 = 1000.0;
pub(crate) fn milliseconds(microseconds: f64) -> f64 {
    microseconds / MICROSECONDS_PER_MILLISECOND
}

/// This relation carries native units; aggregation is a source operation, not another counter interpretation.
pub(crate) fn relation(with_tempdb: bool) -> String {
    let tempdb = if with_tempdb { "r.avg_tempdb_space_used * r.count_executions" } else { "CAST(NULL AS float)" };
    format!(
        r#"WITH culprit_runtime AS (
 SELECT /* culprit */ r.*,
  r.avg_duration*r.count_executions AS culprit_duration_us,
  r.avg_cpu_time*r.count_executions AS culprit_cpu_us,
  r.avg_logical_io_reads*r.count_executions AS culprit_logical_reads,
  r.avg_physical_io_reads*r.count_executions AS culprit_physical_reads,
  r.avg_rowcount*r.count_executions AS culprit_rows,
  {tempdb} AS culprit_tempdb_pages,
  r.count_executions*(SQUARE(r.stdev_duration)+SQUARE(r.avg_duration)) AS culprit_sum_sq_us
 FROM sys.query_store_runtime_stats r
)
"#
    )
}
#[derive(Clone, Debug)]
pub(crate) struct Moments {
    pub executions: u64,
    pub duration_us: f64,
    pub cpu_us: f64,
    pub logical_reads: f64,
    pub physical_reads: f64,
    pub rows: f64,
    pub tempdb_pages: Option<f64>,
    pub sum_sq_us: f64,
    pub min_us: f64,
    pub max_us: f64,
}
impl Moments {
    pub(crate) fn decode(row: &Rec) -> Result<Self> {
        Ok(Self {
            executions: integer(row, "executions")?.try_into().map_err(|_| invalid_data())?,
            duration_us: number(row, "duration_us")?,
            cpu_us: number(row, "cpu_us")?,
            logical_reads: number(row, "logical_reads")?,
            physical_reads: number(row, "physical_reads")?,
            rows: number(row, "row_total")?,
            tempdb_pages: optional_number(row, "tempdb_pages")?,
            sum_sq_us: number(row, "sum_sq_us")?,
            min_us: number(row, "min_us")?,
            max_us: number(row, "max_us")?,
        })
    }
}
#[derive(Clone, Debug)]
pub(crate) struct PlanSummary {
    pub query_id: String,
    pub plan_id: String,
    pub moments: Moments,
    pub first_seen: Option<String>,
    pub last_seen: Option<String>,
    pub forced: bool,
}
impl PlanSummary {
    pub(crate) fn decode(row: &Rec) -> Result<Self> {
        Ok(Self {
            query_id: integer(row, "query_id")?.to_string(),
            plan_id: integer(row, "plan_id")?.to_string(),
            moments: Moments::decode(row)?,
            first_seen: timestamp(row, "first_seen")?,
            last_seen: timestamp(row, "last_seen")?,
            forced: row.bool("is_forced"),
        })
    }
}
fn timestamp(row: &Rec, name: &str) -> Result<Option<String>> {
    match row.str(name) {
        Some(value) => canonical_time(&value).map(Some),
        None => Ok(None),
    }
}
pub(crate) fn summary_sql(with_tempdb: bool) -> String {
    let relation = relation(with_tempdb);
    format!(
        r#"{relation}
SELECT /* culprit */ p.query_id, rs.plan_id,SUM(rs.count_executions) AS executions,
 SUM(rs.culprit_duration_us) AS duration_us, SUM(rs.culprit_cpu_us) AS cpu_us,
 MAX(rs.max_duration) AS max_us,MIN(rs.min_duration) AS min_us,
 SUM(rs.culprit_logical_reads) AS logical_reads,SUM(rs.culprit_physical_reads) AS physical_reads,
 SUM(rs.culprit_rows) AS row_total,SUM(rs.culprit_tempdb_pages) AS tempdb_pages,
 SUM(rs.culprit_sum_sq_us) AS sum_sq_us,
 CONVERT(varchar(40),MIN(rs.first_execution_time),127) AS first_seen,
 CONVERT(varchar(40),MAX(rs.last_execution_time),127) AS last_seen,
 MAX(CAST(p.is_forced_plan AS int)) AS is_forced
FROM culprit_runtime rs
JOIN sys.query_store_runtime_stats_interval i ON i.runtime_stats_interval_id=rs.runtime_stats_interval_id
JOIN sys.query_store_plan p ON p.plan_id=rs.plan_id
JOIN sys.query_store_query q ON q.query_id=p.query_id
JOIN sys.query_store_query_text t ON t.query_text_id=q.query_text_id
WHERE i.end_time>=DATEADD(minute,-@P1,SYSDATETIMEOFFSET()) AND {workload}
GROUP BY p.query_id,rs.plan_id OPTION(MAXDOP 1)"#,
        workload = crate::statement_origin::application_candidates_sql("t.query_sql_text")
    )
}

pub(crate) const LAYOUT: &str = r#"SELECT /* culprit */
 CASE WHEN EXISTS(SELECT 1 FROM sys.all_columns WHERE object_id=OBJECT_ID(N'sys.query_store_runtime_stats') AND name=N'avg_tempdb_space_used') THEN 1 ELSE 0 END AS tempdb,
 CASE WHEN EXISTS(SELECT 1 FROM sys.all_columns WHERE object_id=OBJECT_ID(N'sys.query_store_runtime_stats') AND name=N'replica_group_id') THEN 1 ELSE 0 END AS replicas"#;
