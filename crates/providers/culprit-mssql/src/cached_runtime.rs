//! Completed statement-cache work, with the same native database relation used by grants and workload summaries.
use crate::{
    cached_statements,
    read::{integer, invalid_data, optional_text},
    value::Rec,
};
use culprit_core::{Result, telemetry::*};
use std::collections::BTreeMap;

mod layout;
pub(crate) use layout::Layout;

const STATISTICS: [&str; 4] = ["total", "last", "min", "max"];

fn native_suffix(metric: CachedRuntimeMetric) -> &'static str {
    match metric {
        CachedRuntimeMetric::WorkerMicroseconds => "worker_time",
        CachedRuntimeMetric::ElapsedMicroseconds => "elapsed_time",
        CachedRuntimeMetric::ClrMicroseconds => "clr_time",
        CachedRuntimeMetric::LogicalReads => "logical_reads",
        CachedRuntimeMetric::PhysicalReads => "physical_reads",
        CachedRuntimeMetric::LogicalWrites => "logical_writes",
        CachedRuntimeMetric::Rows => "rows",
        CachedRuntimeMetric::SpilledPages => "spills",
    }
}
pub(crate) fn select_sql(layout: &Layout) -> String {
    let counters = CachedRuntimeMetric::ALL
        .into_iter()
        .flat_map(|metric| {
            STATISTICS.map(|statistic| {
                let column = format!("{statistic}_{}", native_suffix(metric));
                if layout.supports(metric) {
                    format!("qs.{column}")
                } else {
                    format!("CAST(NULL AS bigint) AS {column}")
                }
            })
        })
        .collect::<Vec<_>>()
        .join(",");
    format!(
        r#"SELECT TOP (@P1) {context},qs.execution_count,
 CONVERT(varchar(18),qs.query_plan_hash,1) AS query_plan_hash,
 CONVERT(varchar(40),qs.last_execution_time,126) AS last_execution,
 {counters}
 {relation} WHERE {database} OPTION(MAXDOP 1)"#,
        context = cached_statements::context_columns(),
        relation = cached_statements::relation(),
        database = cached_statements::DATABASE_PREDICATE
    )
}
pub(crate) fn decode(row: &Rec) -> Result<CachedStatementRuntime> {
    let context = cached_statements::context(row)?;
    let measured = context.identity.has_plan_handle();
    let mut counters = BTreeMap::new();
    if measured {
        for metric in CachedRuntimeMetric::ALL {
            if let Some(distribution) = distribution(row, metric)? {
                counters.insert(metric, distribution);
            }
        }
    }
    let value = CachedStatementRuntime {
        context,
        query_plan_hash: optional_text(row, "query_plan_hash")?,
        executions: measured.then(|| integer(row, "execution_count").map(|value| value.to_string())).transpose()?,
        counters,
        last_execution_without_offset: optional_text(row, "last_execution")?
            .map(|at| crate::sql_time::native_time(&at))
            .transpose()?,
        reset: culprit_core::ResetMarker::Unknown,
    };
    if !value.is_valid() {
        return Err(invalid_data());
    }
    Ok(value)
}

fn distribution(row: &Rec, metric: CachedRuntimeMetric) -> Result<Option<CachedCounterDistribution>> {
    let [total, last, minimum, maximum] = STATISTICS
        .map(|statistic| crate::read::optional_counter(row, &format!("{statistic}_{}", native_suffix(metric))));
    match [total?, last?, minimum?, maximum?] {
        [None, None, None, None] if !metric.is_required() => Ok(None),
        [Some(total), Some(last), Some(minimum), Some(maximum)] => {
            Ok(Some(CachedCounterDistribution { total, last, minimum, maximum }))
        }
        _ => Err(invalid_data()),
    }
}

pub(crate) struct Summary {
    pub query_hash: String,
    pub counts: Option<SummaryCounts>,
    pub first_seen_utc_estimate: String,
    pub last_seen_utc_estimate: String,
    pub oldest_entry_age_seconds: i64,
}
pub(crate) struct SummaryCounts {
    pub executions: u64,
    pub duration_us: u64,
    pub maximum_us: u64,
    pub minimum_us: u64,
    pub worker_us: u64,
    pub logical_reads: u64,
    pub physical_reads: u64,
    pub rows: u64,
    pub plans: u32,
}
impl Summary {
    pub(super) fn decode(row: &Rec) -> Result<Self> {
        let query_hash = crate::read::text(row, "query_hash")?;
        if !cached_statements::valid_hash(&query_hash) {
            return Err(invalid_data());
        }
        let counts = if integer(row, "unqualified_entries")? > 0 {
            None
        } else {
            let counter = |name| integer(row, name).map(|value| value as u64);
            Some(SummaryCounts {
                executions: counter("executions")?,
                duration_us: counter("duration_us")?,
                maximum_us: counter("maximum_us")?,
                minimum_us: counter("minimum_us")?,
                worker_us: counter("worker_us")?,
                logical_reads: counter("logical_reads")?,
                physical_reads: counter("physical_reads")?,
                rows: counter("row_total")?,
                plans: integer(row, "plan_count")?.try_into().map_err(|_| invalid_data())?,
            })
        };
        Ok(Self {
            query_hash,
            counts,
            first_seen_utc_estimate: canonical_time(&crate::read::text(row, "first_seen")?)?,
            last_seen_utc_estimate: canonical_time(&crate::read::text(row, "last_seen")?)?,
            oldest_entry_age_seconds: crate::read::signed_integer(row, "age_seconds")?,
        })
    }
}
pub(crate) async fn summary<R: crate::read::NativeRead>(reader: &mut R) -> Result<Vec<Summary>> {
    crate::read::rows(reader, &summary_sql()).await?.iter().map(Summary::decode).collect()
}
pub(crate) fn summary_sql() -> String {
    format!(
        r#"WITH db_stats AS (
 SELECT qs.* {relation} WHERE {database} AND {application}
)
SELECT CONVERT(varchar(18),query_hash,1) AS query_hash,
 SUM(execution_count) AS executions, SUM(total_elapsed_time) AS duration_us,
 MAX(max_elapsed_time) AS maximum_us, MIN(min_elapsed_time) AS minimum_us,
 SUM(total_worker_time) AS worker_us, SUM(total_logical_reads) AS logical_reads,
 SUM(total_physical_reads) AS physical_reads, SUM(total_rows) AS row_total,
 COUNT(DISTINCT query_plan_hash) AS plan_count,
 SUM(CASE WHEN plan_handle={zero_handle} THEN CAST(1 AS bigint) ELSE 0 END) AS unqualified_entries,
 {first} AS first_seen, {last} AS last_seen,
 DATEDIFF(second,MIN(creation_time),GETDATE()) AS age_seconds
FROM db_stats GROUP BY query_hash OPTION(MAXDOP 1)"#,
        relation = cached_statements::relation(),
        database = cached_statements::DATABASE_PREDICATE,
        application = crate::statement_origin::application_candidates_sql("st.text"),
        zero_handle = format_args!("0x{}", "00".repeat(64)),
        first = crate::sql_time::utc_local_iso("MIN(creation_time)"),
        last = crate::sql_time::utc_local_iso("MAX(last_execution_time)")
    )
}

#[cfg(test)]
mod native_tests;
#[cfg(test)]
pub(crate) mod tests;
