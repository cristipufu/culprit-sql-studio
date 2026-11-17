//! All columns belong to the existing Query Store query row, with the same native incarnation and lifetime.
use crate::{
    read::{optional_counter, optional_nonnegative_number},
    value::Rec,
};
use culprit_core::{
    Result,
    telemetry::{MssqlCompilationStage, MssqlCompilationTimings},
};

// The six footnote-2 fields below are constant zero on Synapse, not measured zero timing. The other three
// durations have no such sentinel. Keep native edition dispatch in this fixed provider projection.
// https://learn.microsoft.com/sql/relational-databases/system-catalog-views/sys-query-store-query-transact-sql
const SYNAPSE_STAGE_TIMING_SENTINELS: &str = "CAST(SERVERPROPERTY('EngineEdition') AS int)=6";

pub(super) fn columns() -> String {
    format!(
        r#"q.last_compile_duration,
 CASE WHEN {sentinel} THEN CAST(NULL AS float) ELSE q.avg_bind_duration END AS avg_bind_duration,
 CASE WHEN {sentinel} THEN CAST(NULL AS bigint) ELSE q.last_bind_duration END AS last_bind_duration,
 CASE WHEN {sentinel} THEN CAST(NULL AS float) ELSE q.avg_bind_cpu_time END AS avg_bind_cpu_time,
 CASE WHEN {sentinel} THEN CAST(NULL AS bigint) ELSE q.last_bind_cpu_time END AS last_bind_cpu_time,
 q.avg_optimize_duration,q.last_optimize_duration,
 CASE WHEN {sentinel} THEN CAST(NULL AS float) ELSE q.avg_optimize_cpu_time END AS avg_optimize_cpu_time,
 CASE WHEN {sentinel} THEN CAST(NULL AS bigint) ELSE q.last_optimize_cpu_time END AS last_optimize_cpu_time"#,
        sentinel = SYNAPSE_STAGE_TIMING_SENTINELS
    )
}

pub(super) fn decode(row: &Rec) -> Result<MssqlCompilationTimings> {
    Ok(MssqlCompilationTimings {
        last_duration_us: optional_counter(row, "last_compile_duration")?,
        binding: MssqlCompilationStage {
            average_duration_us: optional_nonnegative_number(row, "avg_bind_duration")?,
            last_duration_us: optional_counter(row, "last_bind_duration")?,
            average_cpu_us: optional_nonnegative_number(row, "avg_bind_cpu_time")?,
            last_cpu_us: optional_counter(row, "last_bind_cpu_time")?,
        },
        optimization: MssqlCompilationStage {
            average_duration_us: optional_nonnegative_number(row, "avg_optimize_duration")?,
            last_duration_us: optional_counter(row, "last_optimize_duration")?,
            average_cpu_us: optional_nonnegative_number(row, "avg_optimize_cpu_time")?,
            last_cpu_us: optional_counter(row, "last_optimize_cpu_time")?,
        },
    })
}

#[cfg(test)]
mod tests;
