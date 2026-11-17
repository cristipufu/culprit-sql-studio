//! Optional request-lifetime native counters use the same held metadata/profile authority as their request.
//! https://learn.microsoft.com/sql/relational-databases/system-dynamic-management-views/sys-dm-exec-requests-transact-sql
use crate::{
    read::{invalid_data, optional_counter},
    value::{Rec, Val},
};
use culprit_core::Result;
pub(crate) use culprit_core::telemetry::PageServerReadAvailability as Availability;

pub(crate) const HYPERSCALE: &str = "CAST(SERVERPROPERTY('EngineEdition') AS int)=5 AND CONVERT(nvarchar(128),DATABASEPROPERTYEX(DB_NAME(),'Edition'))=N'Hyperscale'";
// One exact native system-view object/name/type. An invisible catalog field cannot authorize its projection.
pub(crate) const CAPABILITY: &str = "CASE WHEN CAST(SERVERPROPERTY('EngineEdition') AS int) IN(2,3,4,6,8,9,11,12) THEN 0 WHEN CAST(SERVERPROPERTY('EngineEdition') AS int)=5 THEN CASE WHEN DATABASEPROPERTYEX(DB_NAME(),'Edition') IS NULL THEN 2 WHEN CONVERT(nvarchar(128),DATABASEPROPERTYEX(DB_NAME(),'Edition'))<>N'Hyperscale' THEN 0 WHEN EXISTS(SELECT 1 FROM sys.all_columns WHERE object_id=OBJECT_ID(N'sys.dm_exec_requests') AND name=N'page_server_reads' AND system_type_id=TYPE_ID(N'bigint')) THEN 1 ELSE 3 END ELSE 2 END";

pub(crate) fn availability(row: &Rec) -> Result<Availability> {
    decode(row, "request_page_server_read_availability")
}
pub(crate) fn capability(row: &Rec) -> Result<Availability> {
    decode(row, "request_page_server_read_capability")
}
fn decode(row: &Rec, column: &str) -> Result<Availability> {
    match row.columns().find(|(name, _)| *name == column).map(|(_, value)| value) {
        None | Some(Val::Null) => Ok(Availability::Unknown),
        Some(Val::Int(0)) => Ok(Availability::NotApplicable),
        Some(Val::Int(1)) => Ok(Availability::Available),
        Some(Val::Int(2)) => Ok(Availability::Unknown),
        Some(Val::Int(3)) => Ok(Availability::Unavailable),
        _ => Err(invalid_data()),
    }
}

pub(crate) fn current_availability(availability: Availability) -> String {
    match availability {
        Availability::Available => format!("CASE WHEN {HYPERSCALE} THEN 1 ELSE 2 END"),
        Availability::NotApplicable => "0".into(),
        Availability::Unknown => "2".into(),
        Availability::Unavailable => "3".into(),
    }
}

pub(crate) fn column(availability: Availability, alias: &str) -> String {
    match availability {
        Availability::Available => {
            format!("CASE WHEN {HYPERSCALE} THEN {alias}.page_server_reads ELSE CAST(NULL AS bigint) END")
        }
        _ => "CAST(NULL AS bigint)".into(),
    }
}

pub(crate) fn count(row: &Rec) -> Result<Option<String>> {
    let availability = availability(row)?;
    let capability = capability(row)?;
    let present = row.columns().any(|(name, _)| name == "request_page_server_reads");
    let value = if present { optional_counter(row, "request_page_server_reads")? } else { None };
    if value.is_some() && (availability != Availability::Available || capability != Availability::Available) {
        return Err(invalid_data());
    }
    Ok(value)
}
