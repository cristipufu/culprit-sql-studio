//! Optional same-dispatch local-AG metadata. No independent reader, result set or geo-primary inference.
use super::*;

// Provider protocol states are distinct from all native role/counter codes.
const UNSUPPORTED_PLATFORM: i64 = 0;
const NO_AVAILABILITY_GROUP: i64 = 1;
const LOCAL_AVAILABILITY_GROUP: i64 = 2;
const UNSUPPORTED_LAYOUT: i64 = 3;
const PERMISSION_DENIED: i64 = 4;
const VISIBILITY_UNAVAILABLE: i64 = 5;
const METADATA_UNAVAILABLE: i64 = 6;
const UNQUALIFIED_OBSERVER: i64 = 7;
const MISSING_LOCAL_ROW: i64 = 8;
const AMBIGUOUS_LOCAL_ROWS: i64 = 9;
const NATIVE_READ_FAILED: i64 = 10;
const HADR_INTERFACE: i64 = 1;
const AZURE_INTERFACE: i64 = 2;
const LOCAL_ROW_LOOKAHEAD: usize = 2;

const MEMBERSHIP: &str = "SELECT /* culprit */ @catalog_id=d.database_id,@catalog_state=d.state,\
 @replica_id=d.replica_id,@group_database_id=d.group_database_id \
 FROM sys.databases d WHERE d.name=@database_name OPTION(MAXDOP 1)";
const MEMBERSHIP_PARAMETERS: &str = "N'@database_name sysname,@catalog_id int OUTPUT,@catalog_state int OUTPUT,\
 @replica_id uniqueidentifier OUTPUT,@group_database_id uniqueidentifier OUTPUT',\
 @database_name=@culprit_properties_database_name,@catalog_id=@culprit_role_catalog_id OUTPUT,\
 @catalog_state=@culprit_role_catalog_state OUTPUT,@replica_id=@culprit_role_replica_id OUTPUT,\
 @group_database_id=@culprit_role_group_database_id OUTPUT";
const ROLE_PARAMETERS: &str = "N'@database_id int,@replica_id uniqueidentifier,@group_database_id uniqueidentifier,\
 @rows int OUTPUT,@is_primary bit OUTPUT',@database_id=@culprit_properties_database_id,\
 @replica_id=@culprit_role_replica_id,@group_database_id=@culprit_role_group_database_id,\
 @rows=@culprit_role_rows OUTPUT,@is_primary=@culprit_role_is_primary OUTPUT";

fn native_role_sql(interface: &str) -> String {
    format!(
        "SELECT /* culprit */ @rows=COUNT(*),@is_primary=CONVERT(bit,MIN(CONVERT(int,p.is_primary_replica))) \
         FROM(SELECT TOP({LOCAL_ROW_LOOKAHEAD}) is_primary_replica FROM {interface} \
         WHERE database_id=@database_id AND is_local=1 AND replica_id=@replica_id \
         AND group_database_id=@group_database_id) p OPTION(MAXDOP 1)"
    )
}

pub(super) fn batch() -> String {
    let membership = MEMBERSHIP.replace('\'', "''");
    let hadr = native_role_sql("sys.dm_hadr_database_replica_states").replace('\'', "''");
    let azure = native_role_sql("sys.dm_database_replica_states").replace('\'', "''");
    let observer = crate::replica::ID_EXPRESSION;
    format!(
        r#"DECLARE /* culprit */ @culprit_properties_database_id int=DB_ID(),@culprit_properties_database_name sysname=DB_NAME(),
 @culprit_role_state int={UNSUPPORTED_PLATFORM},@culprit_role_interface int=NULL,
 @culprit_role_observed_at varchar(41)=CONVERT(varchar(40),SYSUTCDATETIME(),127)+'Z',
 @culprit_role_error int=NULL,@culprit_role_catalog_id int=NULL,@culprit_role_catalog_state int=NULL,
 @culprit_role_replica_id uniqueidentifier=NULL,@culprit_role_group_database_id uniqueidentifier=NULL,
 @culprit_role_observer_replica_id uniqueidentifier=NULL,@culprit_role_rows int=NULL,@culprit_role_is_primary bit=NULL,
 @culprit_role_edition int=CONVERT(int,SERVERPROPERTY('EngineEdition')),
 @culprit_role_major int=TRY_CONVERT(int,SERVERPROPERTY('ProductMajorVersion')),
 @culprit_role_visibility int=HAS_PERMS_BY_NAME(DB_NAME(),'DATABASE','VIEW DEFINITION'),
 @culprit_role_permission int=NULL,@culprit_role_sql nvarchar(max)=NULL;
 BEGIN TRY
 IF @culprit_role_edition IN(2,3,4,5,8) AND @culprit_role_major>=11
 BEGIN
  IF COL_LENGTH(N'sys.databases',N'replica_id')<>16 OR COL_LENGTH(N'sys.databases',N'group_database_id')<>16
   OR COL_LENGTH(N'sys.databases',N'replica_id') IS NULL OR COL_LENGTH(N'sys.databases',N'group_database_id') IS NULL
   SET /* culprit */ @culprit_role_state={UNSUPPORTED_LAYOUT};
  ELSE
  BEGIN
   EXEC /* culprit */ sys.sp_executesql N'{membership}',{MEMBERSHIP_PARAMETERS};
   IF @culprit_role_catalog_id IS NULL OR @culprit_role_visibility IS NULL OR @culprit_role_visibility<>1
    SET /* culprit */ @culprit_role_state={METADATA_UNAVAILABLE};
   ELSE IF @culprit_role_edition IN(2,3,4,8) AND @culprit_role_catalog_state=0
    AND @culprit_role_replica_id IS NULL AND @culprit_role_group_database_id IS NULL
    SET /* culprit */ @culprit_role_state={NO_AVAILABILITY_GROUP};
   ELSE IF @culprit_role_replica_id IS NULL OR @culprit_role_group_database_id IS NULL
    SET /* culprit */ @culprit_role_state={METADATA_UNAVAILABLE};
   ELSE
   BEGIN
    IF @culprit_role_edition=5
    BEGIN
     SET /* culprit */ @culprit_role_interface={AZURE_INTERFACE};
     SET /* culprit */ @culprit_role_observer_replica_id={observer};
     SET /* culprit */ @culprit_role_permission=HAS_PERMS_BY_NAME(DB_NAME(),'DATABASE','VIEW DATABASE STATE');
     IF COL_LENGTH(N'sys.dm_database_replica_states',N'database_id')=4
      AND COL_LENGTH(N'sys.dm_database_replica_states',N'is_local')=1
      AND COL_LENGTH(N'sys.dm_database_replica_states',N'is_primary_replica')=1
      AND COL_LENGTH(N'sys.dm_database_replica_states',N'replica_id')=16
      AND COL_LENGTH(N'sys.dm_database_replica_states',N'group_database_id')=16
      SET /* culprit */ @culprit_role_sql=N'{azure}';
    END
    ELSE
    BEGIN
     SET /* culprit */ @culprit_role_interface={HADR_INTERFACE};
     SET /* culprit */ @culprit_role_permission=CASE WHEN @culprit_role_major>=16
      THEN HAS_PERMS_BY_NAME(NULL,NULL,'VIEW SERVER PERFORMANCE STATE')
      ELSE HAS_PERMS_BY_NAME(NULL,NULL,'VIEW SERVER STATE') END;
     IF COL_LENGTH(N'sys.dm_hadr_database_replica_states',N'database_id')=4
      AND COL_LENGTH(N'sys.dm_hadr_database_replica_states',N'is_local')=1
      AND COL_LENGTH(N'sys.dm_hadr_database_replica_states',N'is_primary_replica')=1
      AND COL_LENGTH(N'sys.dm_hadr_database_replica_states',N'replica_id')=16
      AND COL_LENGTH(N'sys.dm_hadr_database_replica_states',N'group_database_id')=16
      SET /* culprit */ @culprit_role_sql=N'{hadr}';
    END;
    IF @culprit_role_permission=0 SET /* culprit */ @culprit_role_state={PERMISSION_DENIED};
    ELSE IF @culprit_role_permission IS NULL SET /* culprit */ @culprit_role_state={VISIBILITY_UNAVAILABLE};
    ELSE IF @culprit_role_interface={AZURE_INTERFACE} AND (@culprit_role_observer_replica_id IS NULL
     OR @culprit_role_observer_replica_id<>@culprit_role_replica_id)
     SET /* culprit */ @culprit_role_state={UNQUALIFIED_OBSERVER};
    ELSE IF @culprit_role_sql IS NULL SET /* culprit */ @culprit_role_state={UNSUPPORTED_LAYOUT};
    ELSE
    BEGIN
     EXEC /* culprit */ sys.sp_executesql @culprit_role_sql,{ROLE_PARAMETERS};
     SET /* culprit */ @culprit_role_state=CASE WHEN @culprit_role_rows=1 THEN {LOCAL_AVAILABILITY_GROUP}
      WHEN @culprit_role_rows=0 THEN {MISSING_LOCAL_ROW} ELSE {AMBIGUOUS_LOCAL_ROWS} END;
    END;
   END;
  END;
 END;
 END TRY
 BEGIN CATCH
  SET /* culprit */ @culprit_role_error=ERROR_NUMBER();
  SET /* culprit */ @culprit_role_state=CASE WHEN @culprit_role_error IN(207,208) THEN {UNSUPPORTED_LAYOUT}
   WHEN @culprit_role_error IN(229,297,300) THEN {PERMISSION_DENIED} ELSE {NATIVE_READ_FAILED} END;
 END CATCH;"#
    )
}

pub(super) fn columns() -> &'static str {
    "@culprit_role_state AS local_role_state,@culprit_role_interface AS local_role_interface,\
 @culprit_role_observed_at AS local_role_observed_at,@culprit_role_error AS local_role_error_number,\
 @culprit_role_catalog_id AS local_role_catalog_id,@culprit_role_catalog_state AS local_role_catalog_state,\
 @culprit_role_visibility AS local_role_metadata_visibility,@culprit_role_rows AS local_role_rows,\
 CONVERT(varchar(36),@culprit_role_replica_id) AS local_role_replica_id,\
 CONVERT(varchar(36),@culprit_role_group_database_id) AS local_role_group_database_id,\
 CONVERT(varchar(36),@culprit_role_observer_replica_id) AS local_role_observer_replica_id,\
 @culprit_role_is_primary AS local_role_is_primary,"
}

pub(super) fn decode(row: &Rec) -> Result<MssqlDatabaseReplicaRole> {
    let observed_at = text(row, "local_role_observed_at")?;
    canonical_time(&observed_at)?;
    let state = integer(row, "local_role_state")?;
    let native_error_number = optional_integer(row, "local_role_error_number")?
        .map(|number| u32::try_from(number).map_err(|_| invalid_data()))
        .transpose()?;
    match state {
        NO_AVAILABILITY_GROUP => no_availability_group(row, observed_at, native_error_number),
        LOCAL_AVAILABILITY_GROUP => local(row, observed_at, native_error_number),
        _ => unavailable_role(state, observed_at, native_error_number),
    }
}

fn no_availability_group(
    row: &Rec,
    observed_at: String,
    native_error_number: Option<u32>,
) -> Result<MssqlDatabaseReplicaRole> {
    if native_error_number.is_some()
        || !matches!(integer(row, "native_engine_edition")?, 2..=4 | 8)
        || optional_integer(row, "local_role_metadata_visibility")? != Some(1)
        || optional_integer(row, "local_role_catalog_state")? != Some(0)
        || optional_integer(row, "local_role_catalog_id")? != optional_integer(row, "catalog_database_id")?
        || crate::replica::id(row, "local_role_replica_id")?.is_some()
        || crate::replica::id(row, "local_role_group_database_id")?.is_some()
    {
        return Err(invalid_data());
    }
    Ok(MssqlDatabaseReplicaRole::NotInAvailabilityGroup { observed_at })
}

fn local(row: &Rec, observed_at: String, native_error_number: Option<u32>) -> Result<MssqlDatabaseReplicaRole> {
    use MssqlDatabaseReplicaRoleSource as S;
    let source = match (integer(row, "local_role_interface")?, integer(row, "native_engine_edition")?) {
        (HADR_INTERFACE, 2..=4 | 8) => S::HadrDatabaseReplicaStates,
        (AZURE_INTERFACE, 5) => S::DatabaseReplicaStates,
        _ => return Err(invalid_data()),
    };
    let replica_id = crate::replica::id(row, "local_role_replica_id")?.ok_or_else(invalid_data)?;
    if native_error_number.is_some()
        || optional_integer(row, "local_role_metadata_visibility")? != Some(1)
        || optional_integer(row, "local_role_rows")? != Some(1)
        || optional_integer(row, "local_role_catalog_id")? != optional_integer(row, "catalog_database_id")?
        || (source == S::DatabaseReplicaStates
            && !crate::replica::id(row, "local_role_observer_replica_id")?
                .is_some_and(|observer| observer.eq_ignore_ascii_case(&replica_id)))
    {
        return Err(invalid_data());
    }
    Ok(MssqlDatabaseReplicaRole::LocalAvailabilityGroup {
        source,
        observed_at,
        replica_id,
        group_database_id: crate::replica::id(row, "local_role_group_database_id")?.ok_or_else(invalid_data)?,
        is_primary: optional_boolean(row, "local_role_is_primary")?,
    })
}

fn unavailable_role(
    state: i64,
    observed_at: String,
    native_error_number: Option<u32>,
) -> Result<MssqlDatabaseReplicaRole> {
    use MssqlDatabaseReplicaRoleUnavailable as R;
    let reason = match state {
        UNSUPPORTED_PLATFORM => R::UnsupportedPlatform,
        UNSUPPORTED_LAYOUT => R::UnsupportedLayout,
        PERMISSION_DENIED => R::PermissionDenied,
        VISIBILITY_UNAVAILABLE => R::VisibilityUnavailable,
        METADATA_UNAVAILABLE => R::MetadataUnavailable,
        UNQUALIFIED_OBSERVER => R::UnqualifiedObserver,
        MISSING_LOCAL_ROW => R::MissingLocalRow,
        AMBIGUOUS_LOCAL_ROWS => R::AmbiguousLocalRows,
        NATIVE_READ_FAILED => R::NativeReadFailed,
        _ => return Err(invalid_data()),
    };
    Ok(MssqlDatabaseReplicaRole::Unavailable { observed_at, reason, native_error_number })
}

#[cfg(test)]
mod tests;
