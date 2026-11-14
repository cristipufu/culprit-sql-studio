use crate::value::Rec;
use culprit_core::{
    Result,
    telemetry::{
        MeasurementValue, NativeIdentity, NativeMeasurementKey, NativePlanIdentity, NativeQueryIdentity,
        RuntimeMeasurement, RuntimeMetric, RuntimeMetricKey, RuntimeMetricUnit, StatementReferences, TelemetryRecord,
        TelemetryStream, WaitMeasurement, canonical_time,
    },
};

pub(crate) use crate::read::{integer, invalid_data as malformed, text};

use crate::read::{optional_nonnegative_number as optional_number, required_nonnegative_number as number};

pub(crate) fn query_identity(row: &Rec) -> Result<NativeQueryIdentity> {
    prefixed_query_identity(row, "")
}

pub(crate) fn prefixed_query_identity(row: &Rec, prefix: &str) -> Result<NativeQueryIdentity> {
    let field = |name: &str| format!("{prefix}{name}");
    Ok(NativeQueryIdentity {
        query_id: integer(row, &field("query_id"))?.to_string(),
        qualifiers: [
            ("context_settings_id".into(), integer(row, &field("context_settings_id"))?.to_string()),
            ("object_id".into(), integer(row, &field("object_id"))?.to_string()),
            ("query_hash".into(), text(row, &field("query_hash"))?),
            ("initial_compile_at".into(), canonical_time(&text(row, &field("query_initial_compile_at"))?)?),
        ]
        .into(),
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
    plan_identity_for_query(row, "", query_identity(row)?)
}

pub(crate) fn plan_identity_for_query(
    row: &Rec,
    prefix: &str,
    query: NativeQueryIdentity,
) -> Result<Option<NativePlanIdentity>> {
    let field = |name: &str| format!("{prefix}{name}");
    if ["definition_plan_id", "plan_hash", "plan_initial_compile_at"]
        .into_iter()
        .any(|name| row.str(&field(name)).is_none())
    {
        return Ok(None);
    }
    Ok(Some(NativePlanIdentity {
        plan_id: integer(row, &field("definition_plan_id"))?.to_string(),
        query,
        qualifiers: [
            ("plan_hash".into(), text(row, &field("plan_hash"))?),
            ("initial_compile_at".into(), canonical_time(&text(row, &field("plan_initial_compile_at"))?)?),
        ]
        .into(),
    }))
}

pub(crate) const SQL_SERVER_PAGE_BYTES: u32 = 8192;

pub(crate) fn record(row: &Rec, stream: TelemetryStream, references: StatementReferences) -> Result<TelemetryRecord> {
    let outcome: u8 = integer(row, "execution_type")?.try_into().map_err(|_| malformed())?;
    let key = NativeMeasurementKey {
        native: NativeIdentity::new(integer(row, "row_id")?.to_string())
            .qualified("plan_id", integer(row, "plan_id")?.to_string())
            .qualified("replica_group_id", integer(row, "replica_group_id")?.to_string())
            .qualified("execution_type", outcome.to_string()),
        references,
    };
    Ok(match stream {
        TelemetryStream::QueryRuntime => TelemetryRecord::QueryRuntime(RuntimeMeasurement {
            key,
            executions: integer(row, "count_executions")?.to_string(),
            metrics: runtime_metrics(row)?,
            first_execution_at: Some(text(row, "first_execution_at")?),
            last_execution_at: Some(text(row, "last_execution_at")?),
        }),
        TelemetryStream::QueryWaits => TelemetryRecord::QueryWaits(WaitMeasurement {
            key,
            category: u8::try_from(integer(row, "wait_category")?).map_err(|_| malformed())?.to_string(),
            unit: RuntimeMetricUnit::Milliseconds,
            total: MeasurementValue::Integer(integer(row, "total_query_wait_time_ms")?.to_string()),
            average: Some(number(row, "avg_query_wait_time_ms")?),
            minimum: Some(MeasurementValue::Integer(integer(row, "min_query_wait_time_ms")?.to_string())),
            maximum: Some(MeasurementValue::Integer(integer(row, "max_query_wait_time_ms")?.to_string())),
        }),
        _ => return Err(malformed()),
    })
}
fn runtime_metrics(row: &Rec) -> Result<Vec<RuntimeMetric>> {
    use RuntimeMetricKey as Key;
    use RuntimeMetricUnit as Unit;
    let pages = Unit::Pages { bytes_per_page: SQL_SERVER_PAGE_BYTES };
    let mut duration = RuntimeMetric::average(Key::Duration, Unit::Microseconds, number(row, "avg_duration")?);
    duration.minimum = Some(number(row, "min_duration")?);
    duration.maximum = Some(number(row, "max_duration")?);
    duration.standard_deviation = Some(number(row, "stdev_duration")?);
    let mut metrics = vec![
        duration,
        RuntimeMetric::average(Key::CpuTime, Unit::Microseconds, number(row, "avg_cpu_time")?),
        RuntimeMetric::average(Key::LogicalReads, pages, number(row, "avg_logical_io_reads")?),
        RuntimeMetric::average(Key::PhysicalReads, pages, number(row, "avg_physical_io_reads")?),
    ];
    for (name, key, unit) in [
        ("avg_logical_io_writes", Key::LogicalWrites, pages),
        ("avg_rowcount", Key::Rows, Unit::Count),
        ("avg_dop", Key::Parallelism, Unit::Ratio),
        ("avg_query_max_used_memory", Key::PeakMemory, pages),
        ("avg_clr_time", Key::Native("mssql.clr-time".into()), Unit::Microseconds),
        ("avg_log_bytes_used", Key::LogBytes, Unit::Bytes),
        ("avg_tempdb_space_used", Key::TemporaryStorage, pages),
    ] {
        if let Some(value) = optional_number(row, name)? {
            metrics.push(RuntimeMetric::average(key, unit, value));
        }
    }
    Ok(metrics)
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
