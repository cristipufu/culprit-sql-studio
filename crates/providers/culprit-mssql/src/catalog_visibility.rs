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

/// The selected native catalog projection owns these status codes; NULL data is a separate value.
pub(crate) fn availability(
    row: &crate::value::Rec,
    column: &str,
) -> Result<culprit_core::telemetry::MssqlCatalogAvailability> {
    use culprit_core::telemetry::MssqlCatalogAvailability;
    match crate::read::integer(row, column)? {
        0 => Ok(MssqlCatalogAvailability::Available),
        1 => Ok(MssqlCatalogAvailability::Unsupported),
        2 => Ok(MssqlCatalogAvailability::PermissionDenied),
        3 => Ok(MssqlCatalogAvailability::Unknown),
        _ => Err(invalid_data()),
    }
}

/// A Unicode SQL literal for a provider-owned dynamic catalog projection, not identifier or query parsing.
pub(crate) fn sql_literal(sql: &str) -> String {
    format!("N'{}'", sql.replace('\'', "''"))
}
