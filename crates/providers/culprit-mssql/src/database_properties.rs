//! One finite selected-database projection serves scans, settings and the existing Operational clock read.
use crate::{
    read::{NativeRead, ReadError, integer, invalid_data, optional_boolean, optional_integer, optional_text, text},
    value::Rec,
};
use culprit_core::{Error, ErrorCode, Result, telemetry::*};

const PREFIX: &str = "SELECT /* culprit */ CONVERT(varchar(40),SYSUTCDATETIME(),127)+'Z' AS observed_at,HAS_PERMS_BY_NAME(DB_NAME(),'DATABASE','VIEW DEFINITION') AS metadata_visibility,CAST(0 AS int) AS property_error_number,DB_ID() AS selected_database_id,DB_NAME() AS selected_database,d.database_id,d.name AS database_name,d.state,d.state_desc,d.user_access,d.user_access_desc,CAST(DATABASEPROPERTYEX(DB_NAME(),'Updateability') AS nvarchar(128)) AS updateability,d.is_read_only,d.collation_name,d.compatibility_level,d.snapshot_isolation_state,d.snapshot_isolation_state_desc,d.is_read_committed_snapshot_on,d.is_auto_create_stats_on,d.is_auto_update_stats_on,d.is_auto_update_stats_async_on,d.is_parameterization_forced,";
const SUFFIX: &str = ",d.page_verify_option,d.page_verify_option_desc,d.recovery_model,d.recovery_model_desc,d.target_recovery_time_in_seconds,CONVERT(varchar(23),CONVERT(datetime,DATABASEPROPERTYEX(DB_NAME(),'LastGoodCheckDbTime')),126) AS last_good_checkdb_without_offset FROM (VALUES(1)) AS selected_database(only_row) LEFT JOIN sys.databases AS d ON d.database_id=DB_ID() OPTION(MAXDOP 1)";
const FALLBACK: &str = "SELECT /* culprit */ CONVERT(varchar(40),SYSUTCDATETIME(),127)+'Z' AS observed_at,HAS_PERMS_BY_NAME(DB_NAME(),'DATABASE','VIEW DEFINITION') AS metadata_visibility,DB_NAME() AS selected_database,ERROR_NUMBER() AS property_error_number";

/// Fixed SQL only. Dynamic compilation keeps pre-2016 servers and hidden columns out of an invalid binding;
/// permitted property-detail failures return the original clock shape in the SAME dispatch, never a retry.
pub(crate) fn query() -> String {
    let prefix = PREFIX.replace('\'', "''");
    let suffix = SUFFIX.replace('\'', "''");
    format!(
        "DECLARE /* culprit */ @culprit_database_properties nvarchar(max)=N'{prefix}';\n\
         IF COL_LENGTH(N'sys.databases',N'is_query_store_on') IS NULL\n\
          SET /* culprit */ @culprit_database_properties+=N'CAST(NULL AS bit) AS is_query_store_on';\n\
         ELSE SET /* culprit */ @culprit_database_properties+=N'd.is_query_store_on';\n\
         SET /* culprit */ @culprit_database_properties+=N'{suffix}';\n\
         BEGIN TRY EXEC /* culprit */ sys.sp_executesql @culprit_database_properties; END TRY\n\
         BEGIN CATCH\n\
          IF ERROR_NUMBER() NOT IN(207,208,229,297,300) THROW;\n\
          {FALLBACK};\n\
         END CATCH;"
    )
}

pub(crate) struct Read {
    pub database: String,
    pub observed_at: String,
    pub metadata_visible: bool,
    pub properties: Result<DatabasePropertiesObservation>,
}
pub(crate) async fn read<R: NativeRead>(reader: &mut R) -> Result<Read> {
    let rows = reader.query(&query(), vec![], 2).await.map_err(ReadError::into_error)?;
    let [row] = rows.as_slice() else { return Err(invalid_data()) };
    let observed_at = text(row, "observed_at")?;
    canonical_time(&observed_at)?;
    let metadata_visible = integer(row, "metadata_visibility")? == 1;
    let properties = decode(row, &observed_at);
    Ok(Read { database: text(row, "selected_database")?, observed_at, metadata_visible, properties })
}

fn decode(row: &Rec, at: &str) -> Result<DatabasePropertiesObservation> {
    match integer(row, "property_error_number")? {
        0 => {}
        207 | 208 => return Err(unavailable(ErrorCode::Unsupported)),
        229 | 297 | 300 => return Err(unavailable(ErrorCode::PermissionDenied)),
        _ => return Err(invalid_data()),
    }
    let Some(id) = optional_integer(row, "database_id")? else { return Err(unavailable(ErrorCode::PermissionDenied)) };
    let database = text(row, "database_name")?;
    if id != integer(row, "selected_database_id")? || database != text(row, "selected_database")? {
        return Err(CollectionStop::SourceChanged.error());
    }
    let point = DatabasePropertiesObservation {
        profile: DatabasePropertiesProfile::MssqlSysDatabases,
        database,
        native_database_id: id.to_string(),
        observed_at: at.into(),
        catalog_state: code(row, "state", "state_desc")?,
        user_access: code(row, "user_access", "user_access_desc")?,
        updateability: optional_text(row, "updateability")?,
        catalog_read_only: optional_boolean(row, "is_read_only")?,
        collation: optional_text(row, "collation_name")?,
        compatibility_level: byte(row, "compatibility_level")?,
        snapshot_isolation: code(row, "snapshot_isolation_state", "snapshot_isolation_state_desc")?,
        read_committed_snapshot: optional_boolean(row, "is_read_committed_snapshot_on")?,
        auto_create_statistics: optional_boolean(row, "is_auto_create_stats_on")?,
        auto_update_statistics: optional_boolean(row, "is_auto_update_stats_on")?,
        auto_update_statistics_async: optional_boolean(row, "is_auto_update_stats_async_on")?,
        forced_parameterization: optional_boolean(row, "is_parameterization_forced")?,
        query_store_enabled: optional_boolean(row, "is_query_store_on")?,
        page_verify: code(row, "page_verify_option", "page_verify_option_desc")?,
        recovery_model: code(row, "recovery_model", "recovery_model_desc")?,
        last_good_checkdb_without_offset: optional_text(row, "last_good_checkdb_without_offset")?,
        target_recovery_time_seconds: optional_integer(row, "target_recovery_time_in_seconds")?
            .map(|value| u32::try_from(value).map_err(|_| invalid_data()))
            .transpose()?,
    };
    point.is_valid().then_some(point).ok_or_else(invalid_data)
}
fn code(row: &Rec, number: &str, description: &str) -> Result<DatabasePropertyCode> {
    Ok(DatabasePropertyCode { code: byte(row, number)?, description: optional_text(row, description)? })
}
fn byte(row: &Rec, name: &str) -> Result<Option<u8>> {
    optional_integer(row, name)?.map(|value| u8::try_from(value).map_err(|_| invalid_data())).transpose()
}
fn unavailable(code: ErrorCode) -> Error {
    Error::new(code, culprit_core::t!("view.database_properties.unavailable"))
}

pub(crate) fn observation(point: DatabasePropertiesObservation) -> Result<OperationalObservation> {
    Ok(OperationalObservation {
        source: DATABASE_PROPERTIES_SOURCE.into(),
        key: point.key()?,
        fact: OperationalFact::DatabaseProperties(Box::new(point)),
    })
}

#[cfg(test)]
pub(crate) mod tests;

#[cfg(test)]
mod native_tests;
