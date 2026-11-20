//! One native aggregate supplies the original active subset and the independent all-retained population.
use super::*;
use crate::read::{database_writable, optional_text};
use culprit_core::{Error, ErrorCode, telemetry::*};

const POINT: &str = "CONVERT(varchar(40),SYSUTCDATETIME(),127)+N'Z' AS observed_at,DB_ID() AS native_database_id,DB_NAME() AS database_name,CONVERT(nvarchar(60),DATABASEPROPERTYEX(DB_NAME(),N'Updateability')) AS updateability";
pub(super) fn query() -> String {
    // Dynamic compilation catches absent/hidden system metadata in this same dispatch, not a retry.
    // The documented table retains at most1000 rows and is editable; this is neither complete error history
    // nor a CPU-work guarantee. All table work stays under the existing native read deadline.
    let statement = super::serial_select(&format!(
        "SELECT /* culprit */ {POINT},0 AS native_error,COUNT_BIG(CASE WHEN event_type IN(1,2,3) THEN 1 END) AS count,COUNT_BIG(*) AS retained_rows,CONVERT(varchar(30),MAX(CASE WHEN event_type IN(1,2,3) THEN last_update_date END),126) AS last_native_at FROM msdb.dbo.suspect_pages WHERE database_id=DB_ID()"
    )).replace('\'', "''");
    let unavailable = super::serial_select(&format!(
        "SELECT /* culprit */ {POINT},ERROR_NUMBER() AS native_error,CAST(NULL AS bigint) AS count,CAST(NULL AS bigint) AS retained_rows,CAST(NULL AS varchar(30)) AS last_native_at"
    ));
    format!(
        "BEGIN TRY EXEC /* culprit */ sys.sp_executesql N'{statement}'; END TRY BEGIN CATCH IF ERROR_NUMBER() NOT IN(207,208,229,297,300,916) THROW; {unavailable}; END CATCH;"
    )
}
pub(super) fn decode(rows: &[crate::value::Rec]) -> Result<SuspectPages> {
    let [row] = rows else { return Err(invalid_data()) };
    match integer(row, "native_error")? {
        0 => {}
        207 | 208 => return Err(unavailable(ErrorCode::Unsupported)),
        229 | 297 | 300 | 916 => return Err(unavailable(ErrorCode::PermissionDenied)),
        _ => return Err(invalid_data()),
    }
    let point = MssqlSuspectPageHistory {
        database: text(row, "database_name")?,
        native_database_id: integer(row, "native_database_id")?.to_string(),
        observed_at: canonical_time(&text(row, "observed_at")?)?,
        database_writable: database_writable(row)?,
        count: integer(row, "count")?.to_string(),
        retained_rows: integer(row, "retained_rows")?.to_string(),
        last_reported_time_without_offset: optional_text(row, "last_native_at")?,
        reset: culprit_core::ResetMarker::Unknown,
    };
    point.is_valid().then_some(point).ok_or_else(invalid_data)
}
fn unavailable(code: ErrorCode) -> Error {
    Error::new(code, culprit_core::t!("view.suspect_pages.unavailable"))
}

#[cfg(test)]
pub(crate) mod tests;
