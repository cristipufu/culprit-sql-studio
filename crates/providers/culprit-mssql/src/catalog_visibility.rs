//! Database-wide metadata visibility is required before treating an empty catalog as absence.
use crate::read::{NativeRead, ReadError, invalid_data, optional_boolean};
use culprit_core::Result;
pub(crate) async fn complete<R: NativeRead>(reader: &mut R) -> Result<bool> {
    let rows = reader
        .query("SELECT HAS_PERMS_BY_NAME(DB_NAME(),'DATABASE','VIEW DEFINITION') AS metadata_visibility", vec![], 2)
        .await
        .map_err(ReadError::into_error)?;
    let [row] = rows.as_slice() else { return Err(invalid_data()) };
    Ok(optional_boolean(row, "metadata_visibility")? == Some(true))
}

pub(crate) fn unavailable() -> culprit_core::Error {
    culprit_core::Error::new(
        culprit_core::ErrorCode::PermissionDenied,
        culprit_core::t!("provider.mssql.catalog.metadata_unavailable"),
    )
    .with_hint("GRANT VIEW DEFINITION TO [<user>];")
}
