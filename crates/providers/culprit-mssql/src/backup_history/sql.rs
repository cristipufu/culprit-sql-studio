//! Fixed native profile projections. TOP bounds returned rows, not internal MSDB/DMV work or sorting cost.
use culprit_core::telemetry::MssqlBackupHistoryProfile;

const AZURE_LAYOUT: &[(&str, i32, u8, u8)] = &[
    ("backup_file_id", 36, 0, 0),
    ("logical_database_id", 36, 0, 0),
    ("physical_database_name", 231, 0, 0),
    ("logical_server_name", 231, 0, 0),
    ("logical_database_name", 231, 0, 0),
    ("backup_start_date", 42, 7, 0),
    ("backup_finish_date", 42, 7, 0),
    ("backup_type", 175, 0, 0),
    ("in_retention", 104, 0, 0),
];
const MSDB_LAYOUT: &[(&str, i32, u8, u8)] = &[
    ("backup_set_id", 56, 0, 0),
    ("backup_set_uuid", 36, 0, 0),
    ("database_guid", 36, 0, 0),
    ("database_name", 231, 0, 0),
    ("family_guid", 36, 0, 0),
    ("first_recovery_fork_guid", 36, 0, 0),
    ("last_recovery_fork_guid", 36, 0, 0),
    ("backup_start_date", 61, 0, 0),
    ("backup_finish_date", 61, 0, 0),
    ("time_zone", 52, 0, 0),
    ("type", 175, 0, 0),
    ("backup_size", 108, 0, 20),
    ("compressed_backup_size", 108, 0, 20),
    ("user_name", 231, 0, 0),
    ("server_name", 231, 0, 0),
    ("machine_name", 231, 0, 0),
    ("software_vendor_id", 56, 0, 0),
    ("software_major_version", 48, 0, 0),
    ("software_minor_version", 48, 0, 0),
    ("software_build_version", 52, 0, 0),
    ("first_lsn", 108, 0, 25),
    ("last_lsn", 108, 0, 25),
    ("checkpoint_lsn", 108, 0, 25),
    ("database_backup_lsn", 108, 0, 25),
    ("differential_base_lsn", 108, 0, 25),
    ("fork_point_lsn", 108, 0, 25),
    ("is_copy_only", 104, 0, 0),
    ("is_snapshot", 104, 0, 0),
    ("has_backup_checksums", 104, 0, 0),
    ("is_damaged", 104, 0, 0),
    ("begins_log_chain", 104, 0, 0),
    ("has_incomplete_metadata", 104, 0, 0),
];
const META_COLUMNS: &str = r#"DB_ID() AS database_id,DB_NAME() AS database_name,
 CAST(SERVERPROPERTY('EngineEdition') AS int) AS engine_edition,
 CAST(SERVERPROPERTY('ProductMajorVersion') AS nvarchar(16)) AS major,
 CAST(DATABASEPROPERTYEX(DB_NAME(),'Edition') AS nvarchar(128)) AS database_edition,
 CAST(DATABASEPROPERTYEX(DB_NAME(),'ReplicaID') AS nvarchar(128)) AS replica_id,
 CAST(DATABASEPROPERTYEX(DB_NAME(),'Updateability') AS nvarchar(32)) AS updateability,
 CONVERT(varchar(33),SYSUTCDATETIME(),126)+N'Z' AS observed_at"#;

pub(super) fn metadata(profile: MssqlBackupHistoryProfile) -> String {
    let (catalog, view, columns) = match profile {
        MssqlBackupHistoryProfile::AzureSqlDatabase => ("sys.all_columns", "sys.dm_database_backups", AZURE_LAYOUT),
        MssqlBackupHistoryProfile::SqlServerMsdb | MssqlBackupHistoryProfile::ManagedInstanceMsdb => {
            ("msdb.sys.all_columns", "msdb.dbo.backupset", MSDB_LAYOUT)
        }
    };
    let expected = columns
        .iter()
        .map(|(name, kind, scale, precision)| format!("(N'{name}',{kind},{scale},{precision})"))
        .collect::<Vec<_>>()
        .join(",");
    format!(
        r#"SELECT /* culprit */ {META_COLUMNS},
 CASE WHEN OBJECT_ID(N'{view}') IS NOT NULL AND NOT EXISTS(
  SELECT 1 FROM(VALUES {expected}) expected(name,native_type,native_scale,native_precision)
  LEFT JOIN {catalog} c ON c.object_id=OBJECT_ID(N'{view}') AND c.name=expected.name
  WHERE c.column_id IS NULL OR c.system_type_id<>expected.native_type
   OR (expected.native_type=42 AND c.scale<>expected.native_scale)
   OR (expected.native_type=108 AND (c.scale<>0 OR c.precision<>expected.native_precision))
 ) THEN 1 ELSE 0 END AS layout_available OPTION(MAXDOP 1)"#
    )
}

const FRAME: &str = r#"DECLARE /* culprit */ @culprit_backup_database_id int=DB_ID(),
 @culprit_backup_database_name nvarchar(128)=DB_NAME(),
 @culprit_backup_updateability nvarchar(32)=CONVERT(nvarchar(32),DATABASEPROPERTYEX(DB_NAME(),'Updateability')),
 @culprit_backup_observed_at varchar(33)=CONVERT(varchar(33),SYSUTCDATETIME(),126)+N'Z';"#;
const FRAME_COLUMNS: &str = r#"@culprit_backup_database_id AS selected_database_id,
 @culprit_backup_database_name AS selected_database_name,
 @culprit_backup_updateability AS updateability,@culprit_backup_observed_at AS observed_at"#;

pub(super) fn history(profile: MssqlBackupHistoryProfile) -> String {
    match profile {
        MssqlBackupHistoryProfile::AzureSqlDatabase => {
            let identity =
                crate::database_identity::window_declaration(crate::probe::ENGINE_EDITION_AZURE_SQL_DATABASE);
            format!(
                r#"{FRAME}
{identity}
SELECT TOP(@P1) /* culprit */ {FRAME_COLUMNS},
 @culprit_identity_rows AS native_identity_rows,@culprit_logical_database_guid AS incarnation,
 @culprit_physical_database_guid AS physical_incarnation,
 CAST(5 AS int) AS engine_edition,@culprit_backup_database_name AS database_name,
 b.backup_file_id,b.logical_database_id,b.physical_database_name,b.logical_server_name,b.logical_database_name,
 CONVERT(varchar(33),b.backup_start_date,126) AS started_without_offset,
 CONVERT(varchar(33),b.backup_finish_date,126) AS finished_without_offset,b.backup_type,b.in_retention
FROM sys.dm_database_backups b
WHERE b.logical_database_id=CONVERT(uniqueidentifier,@P2)
ORDER BY b.backup_finish_date DESC,b.backup_file_id ASC OPTION(MAXDOP 1)"#
            )
        }
        MssqlBackupHistoryProfile::SqlServerMsdb | MssqlBackupHistoryProfile::ManagedInstanceMsdb => format!(
            r#"{FRAME}
DECLARE /* culprit */ @culprit_backup_database_guid varchar(36),@culprit_backup_recovery_fork varchar(36);
SELECT /* culprit */ @culprit_backup_database_guid=CONVERT(varchar(36),database_guid),
 @culprit_backup_recovery_fork=CONVERT(varchar(36),recovery_fork_guid)
FROM sys.database_recovery_status WHERE database_id=@culprit_backup_database_id;
SELECT TOP(@P1) /* culprit */ {FRAME_COLUMNS},
 @culprit_backup_database_guid AS selected_database_guid,@culprit_backup_recovery_fork AS selected_recovery_fork_guid,
 b.backup_set_id,b.backup_set_uuid,b.database_guid,b.database_name,
 b.family_guid,b.first_recovery_fork_guid,b.last_recovery_fork_guid,
 CONVERT(varchar(33),CAST(b.backup_start_date AS datetime2),126) AS started_without_offset,
 CONVERT(varchar(33),CAST(b.backup_finish_date AS datetime2),126) AS finished_without_offset,
 b.time_zone,b.type AS backup_type,
 CONVERT(varchar(40),b.backup_size) AS size_bytes,CONVERT(varchar(40),b.compressed_backup_size) AS compressed_size_bytes,
 b.user_name,b.server_name,b.machine_name,
 b.software_vendor_id,b.software_major_version,b.software_minor_version,b.software_build_version,
 CONVERT(varchar(40),b.first_lsn) AS first_lsn,CONVERT(varchar(40),b.last_lsn) AS last_lsn,
 CONVERT(varchar(40),b.checkpoint_lsn) AS checkpoint_lsn,CONVERT(varchar(40),b.database_backup_lsn) AS database_backup_lsn,
 CONVERT(varchar(40),b.differential_base_lsn) AS differential_base_lsn,CONVERT(varchar(40),b.fork_point_lsn) AS fork_point_lsn,
 b.is_copy_only,b.is_snapshot,b.has_backup_checksums,b.is_damaged,b.begins_log_chain,b.has_incomplete_metadata
FROM msdb.dbo.backupset b
WHERE b.database_guid=CONVERT(uniqueidentifier,@P2) AND b.database_name=@P3
ORDER BY b.backup_set_id DESC OPTION(MAXDOP 1)"#
        ),
    }
}
