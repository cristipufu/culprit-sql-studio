//! Rolling Query Store window bounds; length and displayed bounds use the same precision.
use crate::sql_time::{utc_offset_iso, utc_second};
/// The window's end: the open interval's scheduled end is in the future, so it ends now.
pub(crate) const CAPPED_END: &str =
    "CASE WHEN MAX(end_time) > SYSDATETIMEOFFSET() THEN SYSDATETIMEOFFSET() ELSE MAX(end_time) END";

/// The window's bounds, and its length measured between those same bounds: DATEDIFF counts the second boundaries
/// crossed while the bounds round to the second, so a length taken from the unrounded end can be a second short of
/// them, and a length its bounds contradict is read as unknown.
pub(crate) fn window_sql(window_minutes: u32) -> String {
    format!(
        r#"
DECLARE @since datetimeoffset = DATEADD(minute, -{window_minutes}, SYSDATETIMEOFFSET());
SELECT {start} AS window_start, {end} AS window_end,
  DATEDIFF(second, {start_second}, {end_second}) / 3600.0 AS window_hours
FROM sys.query_store_runtime_stats_interval
WHERE end_time >= @since
"#,
        start = utc_offset_iso("MIN(start_time)"),
        end = utc_offset_iso(CAPPED_END),
        start_second = utc_second("MIN(start_time)"),
        end_second = utc_second(CAPPED_END),
    )
}
