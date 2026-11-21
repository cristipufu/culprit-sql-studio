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

/// Fixed optional index/partition column layout, evaluated once per native batch.
pub(crate) fn index_column_capability(
    variable: &str,
    catalog: &str,
    column: &str,
    native_type: &str,
    minimum_major: u32,
    azure: bool,
) -> String {
    let supported = if azure {
        format!("@culprit_index_major>={minimum_major} OR @culprit_index_edition IN(5,8)")
    } else {
        format!("@culprit_index_major>={minimum_major}")
    };
    format!(
        r#"DECLARE /* culprit */ @culprit_{variable} int=CASE
 WHEN @culprit_index_metadata=0 OR HAS_PERMS_BY_NAME(N'{catalog}',N'OBJECT',N'SELECT')=0 THEN 2
 WHEN @culprit_index_metadata IS NULL OR @culprit_index_major IS NULL OR @culprit_index_edition IS NULL
  OR OBJECT_ID(N'{catalog}') IS NULL OR HAS_PERMS_BY_NAME(N'{catalog}',N'OBJECT',N'SELECT') IS NULL THEN 3
 WHEN NOT({supported}) THEN 1
 WHEN NOT EXISTS(SELECT 1 FROM sys.all_columns WHERE object_id=OBJECT_ID(N'{catalog}') AND name=N'{column}') THEN 1
 WHEN NOT EXISTS(SELECT 1 FROM sys.all_columns WHERE object_id=OBJECT_ID(N'{catalog}') AND name=N'{column}' AND system_type_id=TYPE_ID(N'{native_type}')) THEN 3
 WHEN HAS_PERMS_BY_NAME(N'{catalog}',N'OBJECT',N'SELECT',N'{column}',N'COLUMN')=0 THEN 2
 WHEN HAS_PERMS_BY_NAME(N'{catalog}',N'OBJECT',N'SELECT',N'{column}',N'COLUMN')=1 THEN 0
 ELSE 3 END;"#
    )
}
