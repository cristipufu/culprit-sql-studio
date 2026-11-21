//! The existing log projection serves either the current database or the explicit shared tempdb owner.
//! Reuse reason is the native last-checkpoint category, never a transaction or statement attribution.
use super::*;
use crate::read::{boolean, one};

const TEMPDB_LAYOUT: &str = r#"SELECT /* culprit */
 CAST(SERVERPROPERTY('EngineEdition') AS int) AS edition,
 CAST(PARSENAME(CONVERT(varchar(128),SERVERPROPERTY('ProductVersion')),4) AS int) AS major,
 (SELECT database_id FROM sys.databases WHERE database_id=2 AND name=N'tempdb') AS database_id,
 CASE WHEN HAS_PERMS_BY_NAME(N'tempdb','DATABASE','VIEW DEFINITION')=1
  OR HAS_PERMS_BY_NAME(NULL,NULL,'VIEW ANY DEFINITION')=1 THEN 1 ELSE 0 END AS metadata_visible,
 CASE WHEN CAST(SERVERPROPERTY('EngineEdition') AS int)=8
  OR CAST(PARSENAME(CONVERT(varchar(128),SERVERPROPERTY('ProductVersion')),4) AS int)>=16
  THEN CASE WHEN HAS_PERMS_BY_NAME(NULL,NULL,'VIEW SERVER PERFORMANCE STATE')=1 THEN 1 ELSE 0 END
  ELSE CASE WHEN HAS_PERMS_BY_NAME(NULL,NULL,'VIEW SERVER STATE')=1 THEN 1 ELSE 0 END END AS log_permission,
 (SELECT COUNT(*) FROM tempdb.sys.all_columns
  WHERE object_id=OBJECT_ID(N'tempdb.sys.dm_db_log_space_usage')
   AND name IN(N'database_id',N'total_log_size_in_bytes',N'used_log_space_in_bytes')) AS usage_columns,
 (SELECT COUNT(*) FROM tempdb.sys.all_columns
  WHERE object_id=OBJECT_ID(N'tempdb.sys.database_files')
   AND name IN(N'type',N'size',N'max_size',N'growth')) AS file_columns,
 (SELECT COUNT(*) FROM sys.all_columns
  WHERE object_id=OBJECT_ID(N'sys.databases') AND name=N'log_reuse_wait_desc') AS reuse_columns
 OPTION(MAXDOP 1)"#;

pub(super) async fn require_tempdb_layout<R: NativeRead>(io: &mut R) -> Result<()> {
    let row = one(io, TEMPDB_LAYOUT).await?;
    let major = u32::try_from(integer(&row, "major")?).map_err(|_| invalid_data())?;
    if !crate::instance::supports_profile(integer(&row, "edition")?, major) {
        return Err(culprit_core::Error::new(
            culprit_core::ErrorCode::Unsupported,
            culprit_core::t!("collection.unsupported"),
        ));
    }
    if !boolean(&row, "log_permission")? || !boolean(&row, "metadata_visible")? {
        return Err(culprit_core::Error::new(
            culprit_core::ErrorCode::PermissionDenied,
            culprit_core::t!("provider.mssql.tempdb_log.visibility"),
        ));
    }
    if integer(&row, "database_id")? != 2 {
        return Err(CollectionStop::SourceChanged.error());
    }
    if integer(&row, "usage_columns")? != 3
        || integer(&row, "file_columns")? != 4
        || integer(&row, "reuse_columns")? != 1
    {
        return Err(culprit_core::Error::new(
            culprit_core::ErrorCode::Unsupported,
            culprit_core::t!("collection.unsupported"),
        ));
    }
    Ok(())
}

pub(super) fn query(kind: Kind) -> String {
    let (database_name, identity, files, usage, database_id) = if kind == Kind::TempLog {
        (
            "CAST(NULL AS nvarchar(128))",
            "database_id AS database_id,DB_NAME(database_id) AS name,",
            "tempdb.sys.database_files",
            "tempdb.sys.dm_db_log_space_usage",
            "2",
        )
    } else {
        ("DB_NAME()", "", "sys.database_files", "sys.dm_db_log_space_usage", "DB_ID()")
    };
    let catalog_predicate = if kind == Kind::TempLog {
        format!("database_id={database_id}")
    } else {
        crate::database_identity::catalog::predicate("")
    };
    let sql = format!(
        "DECLARE /* culprit */ @observed_at varchar(40)=CONVERT(varchar(40),SYSUTCDATETIME(),127)+'Z';
 SELECT /* culprit */ @observed_at AS observed_at,{database_name} AS database_name,
 {identity}total_log_size_in_bytes AS total,used_log_space_in_bytes AS used,
 (SELECT log_reuse_wait_desc FROM sys.databases WHERE {catalog_predicate}) AS reuse_wait,
 (SELECT MAX(CASE WHEN growth>0 AND(max_size=-1 OR size<max_size) THEN 1 ELSE 0 END) FROM {files} WHERE type=1) AS can_grow,
 (SELECT CASE WHEN MAX(CASE WHEN growth>0 AND max_size=-1 THEN 1 ELSE 0 END)=1 THEN NULL ELSE SUM(CONVERT(bigint,CASE WHEN growth=0 OR max_size<size THEN size ELSE max_size END))*CONVERT(bigint,8192) END FROM {files} WHERE type=1) AS max_bytes
 FROM {usage}"
    );
    if kind == Kind::TempLog { sql } else { crate::database_identity::catalog::bind(&sql) }
}

#[cfg(test)]
mod tests;
