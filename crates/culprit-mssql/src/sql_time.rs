/// Converts a datetimeoffset expression to second-precision UTC ISO 8601 that every JS engine parses.
pub(crate) fn utc_offset_iso(expr: &str) -> String {
    format!("CONVERT(varchar(19), {}, 126) + 'Z'", utc_second(expr))
}

/// Native datetimeoffset precision for telemetry; do not round evidence to display seconds.
pub(crate) fn utc_offset_precise_iso(expr: &str) -> String {
    format!("CONVERT(varchar(40), SWITCHOFFSET({expr}, '+00:00'), 127)")
}

/// A datetimeoffset expression in UTC, rounded to the second (as a cast to `datetime2(0)` rounds).
pub(crate) fn utc_second(expr: &str) -> String {
    format!("CAST(SWITCHOFFSET({expr}, '+00:00') AS datetime2(0))")
}

/// Converts a server-local datetime expression to second-precision UTC ISO 8601.
pub(crate) fn utc_local_iso(expr: &str) -> String {
    format!(
        "CONVERT(varchar(19), CAST(DATEADD(minute, DATEDIFF(minute, GETDATE(), GETUTCDATE()), {expr}) AS datetime2(0)), 126) + 'Z'"
    )
}

/// SQL Server's datetime identities are local native values, not UTC instants. Preserve ticks without inventing a zone.
pub(crate) fn native_time(value: &str) -> culprit_core::Result<String> {
    use chrono::Timelike;
    let value = chrono::NaiveDateTime::parse_from_str(value, "%Y-%m-%dT%H:%M:%S%.f")
        .map_err(|_| crate::read::invalid_data())?;
    const NANOS_PER_NATIVE_TICK: u32 = 100;
    if !value.nanosecond().is_multiple_of(NANOS_PER_NATIVE_TICK) {
        return Err(crate::read::invalid_data());
    }
    Ok(format!("{}.{:07}", value.format("%Y-%m-%dT%H:%M:%S"), value.nanosecond() / NANOS_PER_NATIVE_TICK))
}
