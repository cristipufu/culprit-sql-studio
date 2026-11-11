use crate::value::Rec;
use culprit_core::{
    Result,
    telemetry::{
        NativeMeasurementKey, NativePlanIdentity, NativeQueryIdentity, RuntimeMeasurement, TelemetryRecord,
        TelemetryStream, WaitMeasurement, canonical_time,
    },
};

pub(crate) use crate::read::{integer, invalid_data as malformed, text};

use crate::read::{optional_nonnegative_number as optional_number, required_nonnegative_number as number};

pub(crate) fn query_identity(row: &Rec) -> Result<NativeQueryIdentity> {
    Ok(NativeQueryIdentity {
        query_id: integer(row, "query_id")?.to_string(),
        context_settings_id: integer(row, "context_settings_id")?.to_string(),
        object_id: integer(row, "object_id")?.to_string(),
        query_hash: text(row, "query_hash")?,
        initial_compile_at: canonical_time(&text(row, "query_initial_compile_at")?)?,
    })
}

pub(crate) fn plan_identity(row: &Rec) -> Result<Option<NativePlanIdentity>> {
    if [
        "definition_plan_id",
        "query_id",
        "context_settings_id",
        "object_id",
        "query_hash",
        "plan_hash",
        "query_initial_compile_at",
        "plan_initial_compile_at",
    ]
    .into_iter()
    .any(|name| row.str(name).is_none())
    {
        return Ok(None);
    }
    Ok(Some(NativePlanIdentity {
        plan_id: integer(row, "definition_plan_id")?.to_string(),
        query: query_identity(row)?,
        plan_hash: text(row, "plan_hash")?,
        initial_compile_at: canonical_time(&text(row, "plan_initial_compile_at")?)?,
    }))
}

pub(crate) fn record(
    row: &Rec,
    stream: TelemetryStream,
    plan_definition_id: Option<String>,
) -> Result<TelemetryRecord> {
    let key = NativeMeasurementKey {
        row_id: integer(row, "row_id")?.to_string(),
        plan_id: integer(row, "plan_id")?.to_string(),
        replica_group_id: integer(row, "replica_group_id")?.to_string(),
        execution_type: integer(row, "execution_type")?.try_into().map_err(|_| malformed())?,
        plan_definition_id,
    };
    Ok(match stream {
        TelemetryStream::QueryRuntime => TelemetryRecord::QueryRuntime(RuntimeMeasurement {
            key,
            executions: integer(row, "count_executions")?.to_string(),
            avg_duration_us: number(row, "avg_duration")?,
            stdev_duration_us: number(row, "stdev_duration")?,
            min_duration_us: number(row, "min_duration")?,
            max_duration_us: number(row, "max_duration")?,
            avg_cpu_us: number(row, "avg_cpu_time")?,
            avg_logical_reads_8kb_pages: number(row, "avg_logical_io_reads")?,
            avg_physical_reads_8kb_pages: number(row, "avg_physical_io_reads")?,
            avg_logical_writes_8kb_pages: optional_number(row, "avg_logical_io_writes")?,
            avg_rows: optional_number(row, "avg_rowcount")?,
            avg_dop: optional_number(row, "avg_dop")?,
            avg_max_used_memory_8kb_pages: optional_number(row, "avg_query_max_used_memory")?,
            avg_clr_us: optional_number(row, "avg_clr_time")?,
            avg_log_bytes: optional_number(row, "avg_log_bytes_used")?,
            avg_tempdb_8kb_pages: optional_number(row, "avg_tempdb_space_used")?,
            first_execution_at: text(row, "first_execution_at")?,
            last_execution_at: text(row, "last_execution_at")?,
        }),
        TelemetryStream::QueryWaits => TelemetryRecord::QueryWaits(WaitMeasurement {
            key,
            category: integer(row, "wait_category")?.try_into().map_err(|_| malformed())?,
            total_wait_ms: integer(row, "total_query_wait_time_ms")?.to_string(),
            avg_wait_ms: number(row, "avg_query_wait_time_ms")?,
            min_wait_ms: integer(row, "min_query_wait_time_ms")?.to_string(),
            max_wait_ms: integer(row, "max_query_wait_time_ms")?.to_string(),
        }),
        _ => return Err(malformed()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::Val;
    #[test]
    fn integer_decode_never_rounds_or_fabricates_missing_counters() {
        let exact = Rec::of(&[("counter", Val::Int(9_007_199_254_740_993))]);
        assert_eq!(integer(&exact, "counter").unwrap().to_string(), "9007199254740993");
        assert!(integer(&Rec::default(), "counter").is_err());
        assert!(integer(&Rec::of(&[("counter", Val::Float(1.0))]), "counter").is_err());
        assert!(number(&Rec::of(&[("counter", Val::Float(f64::NAN))]), "counter").is_err());
    }
}
