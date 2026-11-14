//! One native runtime relation and sufficient-statistics interpretation for raw fragments and summaries.
use crate::{
    read::{
        integer, invalid_data, optional_nonnegative_number as optional_number, required_nonnegative_number as number,
    },
    value::Rec,
};
use culprit_core::{
    Result,
    telemetry::{NativePlanIdentity, NativeQueryIdentity, canonical_time},
};

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
    pub identity: NativeQueryIdentity,
    pub plan_identity: NativePlanIdentity,
    pub plan_id: String,
    pub moments: Moments,
    pub costs: ExecutionCosts,
    pub first_seen: Option<String>,
    pub last_seen: Option<String>,
    pub forced: bool,
}
impl PlanSummary {
    pub(crate) fn decode(row: &Rec) -> Result<Self> {
        let identity = super::decode::query_identity(row)
            .map_err(|_| culprit_core::telemetry::CollectionStop::SourceChanged.error())?;
        let plan_identity = super::decode::plan_identity_for_query(row, "", identity.clone())
            .map_err(|_| culprit_core::telemetry::CollectionStop::SourceChanged.error())?
            .filter(|plan| row.str("plan_id").as_deref() == Some(plan.plan_id.as_str()))
            .ok_or_else(|| culprit_core::telemetry::CollectionStop::SourceChanged.error())?;
        let moments = Moments::decode(row)?;
        let costs = ExecutionCosts::decode(row, moments.executions)?;
        Ok(Self {
            identity,
            plan_identity,
            plan_id: integer(row, "plan_id")?.to_string(),
            moments,
            costs,
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
/// Outcomes and log volume belong to exactly the rows that supplied the runtime denominator. A later read can
/// contain a different execution cohort after a native reset, even with the same query identity and count.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ExecutionCosts {
    pub executions: u64,
    pub aborted: u64,
    pub aborted_us: f64,
    pub failed: u64,
    pub log_bytes: Option<f64>,
    outcomes_complete: bool,
    log_complete: bool,
}

impl ExecutionCosts {
    fn decode(row: &Rec, executions: u64) -> Result<Self> {
        let count = |name| integer(row, name)?.try_into().map_err(|_| invalid_data());
        let known: u64 = count("known_executions")?;
        let value = Self {
            executions,
            aborted: count("aborted")?,
            aborted_us: number(row, "aborted_us")?,
            failed: count("failed")?,
            log_bytes: optional_number(row, "log_bytes")?,
            outcomes_complete: known == executions,
            log_complete: match integer(row, "log_complete")? {
                0 => false,
                1 => true,
                _ => return Err(invalid_data()),
            },
        };
        if known > executions
            || value.aborted.checked_add(value.failed).is_none_or(|sum| sum > known)
            || value.aborted == 0 && value.aborted_us > 0.0
            || !value.log_complete && value.log_bytes.is_some()
        {
            return Err(invalid_data());
        }
        Ok(value)
    }

    pub(crate) fn aggregate<'a>(parts: impl IntoIterator<Item = &'a Self>) -> Option<Self> {
        let mut parts = parts.into_iter();
        let mut total = *parts.next()?;
        for part in parts {
            total.executions = total.executions.checked_add(part.executions)?;
            total.aborted = total.aborted.checked_add(part.aborted)?;
            total.failed = total.failed.checked_add(part.failed)?;
            total.aborted_us += part.aborted_us;
            total.log_bytes = total.log_bytes.zip(part.log_bytes).map(|(sum, next)| sum + next);
            total.outcomes_complete &= part.outcomes_complete;
            total.log_complete &= part.log_complete;
        }
        (total.aborted_us.is_finite() && total.log_bytes.is_none_or(f64::is_finite)).then_some(total)
    }

    pub(crate) fn is_complete(&self) -> bool {
        self.outcomes_complete && self.log_complete && self.exact_counts()
    }

    fn exact_counts(&self) -> bool {
        const MAX_EXACT_COUNT: u64 = 1 << 53;
        self.executions <= MAX_EXACT_COUNT
    }

    pub(crate) fn query_costs(&self, total_log_bytes: Option<f64>) -> culprit_core::QueryCosts {
        let exact = self.exact_counts();
        culprit_core::QueryCosts {
            aborted: (exact && (self.outcomes_complete || self.aborted > 0)).then_some(self.aborted as f64),
            aborted_mean_ms: (exact && self.aborted > 0).then(|| milliseconds(self.aborted_us) / self.aborted as f64),
            failed: (exact && (self.outcomes_complete || self.failed > 0)).then_some(self.failed as f64),
            log_bytes: self.log_bytes,
            log_share: self
                .log_bytes
                .zip(total_log_bytes)
                .and_then(|(own, total)| (total > 0.0 && total.is_finite() && own <= total).then_some(own / total)),
            ..Default::default()
        }
    }
}

pub(crate) fn summary_sql(with_tempdb: bool, with_log: bool) -> String {
    let relation = relation(with_tempdb);
    let (log, log_complete) = if with_log {
        (
            "CASE WHEN COUNT_BIG(CASE WHEN rs.count_executions>0 AND rs.avg_log_bytes_used IS NULL THEN 1 END)=0 THEN SUM(CASE WHEN rs.count_executions>0 THEN rs.avg_log_bytes_used*rs.count_executions ELSE 0 END) END",
            "CASE WHEN COUNT_BIG(CASE WHEN rs.count_executions>0 AND rs.avg_log_bytes_used IS NULL THEN 1 END)=0 THEN 1 ELSE 0 END",
        )
    } else {
        ("CAST(NULL AS float)", "1")
    };
    format!(
        r#"{window_declaration}{relation}
SELECT /* culprit */ {identity},{plan_identity}, rs.plan_id,SUM(rs.count_executions) AS executions,
 SUM(rs.culprit_duration_us) AS duration_us, SUM(rs.culprit_cpu_us) AS cpu_us,
 MAX(rs.max_duration) AS max_us,MIN(rs.min_duration) AS min_us,
 SUM(rs.culprit_logical_reads) AS logical_reads,SUM(rs.culprit_physical_reads) AS physical_reads,
 SUM(rs.culprit_rows) AS row_total,SUM(rs.culprit_tempdb_pages) AS tempdb_pages,
 SUM(rs.culprit_sum_sq_us) AS sum_sq_us,
 SUM(CASE WHEN rs.execution_type IN(0,3,4) THEN rs.count_executions ELSE 0 END) AS known_executions,
 SUM(CASE WHEN rs.execution_type=3 THEN rs.count_executions ELSE 0 END) AS aborted,
 SUM(CASE WHEN rs.execution_type=3 THEN rs.culprit_duration_us ELSE 0 END) AS aborted_us,
 SUM(CASE WHEN rs.execution_type=4 THEN rs.count_executions ELSE 0 END) AS failed,
 {log} AS log_bytes,{log_complete} AS log_complete,
 CONVERT(varchar(40),MIN(rs.first_execution_time),127) AS first_seen,
 CONVERT(varchar(40),MAX(rs.last_execution_time),127) AS last_seen,
 MAX(CAST(p.is_forced_plan AS int)) AS is_forced
FROM culprit_runtime rs
JOIN sys.query_store_runtime_stats_interval i ON i.runtime_stats_interval_id=rs.runtime_stats_interval_id
JOIN sys.query_store_plan p ON p.plan_id=rs.plan_id
JOIN sys.query_store_query q ON q.query_id=p.query_id
JOIN sys.query_store_query_text t ON t.query_text_id=q.query_text_id
WHERE {window_membership} AND {workload}
GROUP BY p.query_id,rs.plan_id,q.context_settings_id,q.object_id,q.query_hash,q.initial_compile_start_time,
 p.plan_id,p.query_plan_hash,p.initial_compile_start_time OPTION(MAXDOP 1)"#,
        identity = super::catalog_sql::query_identity_columns!("p.query_id"),
        plan_identity = super::catalog_sql::plan_identity_columns!("p", ""),
        workload = crate::statement_origin::application_candidates_sql("t.query_sql_text"),
        window_declaration = super::window_sql::DECLARATION,
        window_membership = super::window_sql::MEMBERSHIP,
    )
}

#[cfg(test)]
#[path = "runtime/tests.rs"]
mod tests;

pub(crate) const LAYOUT: &str = concat!(
    r#"SELECT /* culprit */
 CASE WHEN EXISTS(SELECT 1 FROM sys.all_columns WHERE object_id=OBJECT_ID(N'sys.query_store_runtime_stats') AND name=N'avg_tempdb_space_used') THEN 1 ELSE 0 END AS tempdb,
 CASE WHEN EXISTS(SELECT 1 FROM sys.all_columns WHERE object_id=OBJECT_ID(N'sys.query_store_runtime_stats') AND name=N'avg_log_bytes_used') THEN 1 ELSE 0 END AS log_bytes,
 CASE WHEN EXISTS(SELECT 1 FROM sys.all_columns WHERE object_id=OBJECT_ID(N'sys.query_store_runtime_stats') AND name=N'replica_group_id') THEN 1 ELSE 0 END AS replicas,
 "#,
    super::variants_sql::readable!(),
    r#" AS variants,
 CASE WHEN OBJECT_ID(N'sys.query_store_wait_stats') IS NOT NULL AND EXISTS(SELECT 1 FROM sys.all_columns WHERE object_id=OBJECT_ID(N'sys.database_query_store_options') AND name=N'wait_stats_capture_mode_desc') THEN 1 ELSE 0 END AS waits"#
);
