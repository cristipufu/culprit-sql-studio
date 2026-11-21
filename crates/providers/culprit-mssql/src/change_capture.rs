//! Shared strict representation helpers for native CDC and change-feed catalogs; no source acquisition.
use crate::{
    read::{integer, invalid_data, optional_text},
    value::Rec,
};
use culprit_core::Result;
pub(crate) fn native_id(row: &Rec, name: &str) -> Result<u32> {
    integer(row, name)?.try_into().map_err(|_| invalid_data())
}
pub(crate) fn native_lsn(row: &Rec, name: &str) -> Result<Option<String>> {
    // CDC's fixed-width native LSN values can include one terminal NUL. Preserve the LSN itself;
    // embedded or repeated terminators still fail the domain validator instead of being cleaned away.
    Ok(optional_text(row, name)?.map(|mut value| {
        if value.ends_with('\0') {
            value.pop();
        }
        value
    }))
}
pub(crate) fn local_time(row: &Rec, name: &str) -> Result<Option<String>> {
    optional_text(row, name)?.map(|value| crate::sql_time::native_time(&value)).transpose()
}
pub(crate) fn layout(object: &str, columns: &[&str]) -> String {
    let names = columns.iter().map(|name| format!("N'{name}'")).collect::<Vec<_>>().join(",");
    format!(
        "CASE WHEN (SELECT COUNT(*) FROM sys.all_columns WHERE object_id=OBJECT_ID(N'{object}') AND name IN({names}))={} THEN 1 ELSE 0 END",
        columns.len()
    )
}
