//! Published DMV/catalog columns only. TOP caps output; it does not claim bounded internal engine work.
use crate::change_capture::layout;
pub(super) fn status() -> String {
    let scan = layout(
        "sys.dm_cdc_log_scan_sessions",
        &[
            "session_id",
            "start_time",
            "end_time",
            "scan_phase",
            "duration",
            "error_count",
            "tran_count",
            "log_record_count",
            "command_count",
            "failed_sessions_count",
            "latency",
            "last_commit_time",
            "last_commit_cdc_time",
            "start_lsn",
            "current_lsn",
            "end_lsn",
        ],
    );
    let errors = layout(
        "sys.dm_cdc_errors",
        &[
            "session_id",
            "phase_number",
            "entry_time",
            "error_number",
            "error_severity",
            "error_state",
            "start_lsn",
            "begin_lsn",
            "sequence_value",
        ],
    );
    let select = |enabled: &str| {
        format!(
            "SELECT /* culprit */ {current_id} AS database_id,CONVERT(varchar(40),SYSUTCDATETIME(),127)+'Z' AS observed_at,CAST({enabled} AS int) AS enabled,HAS_PERMS_BY_NAME({current_name},'DATABASE',@P1) AS has_permission,{scan} AS scan_layout,{errors} AS error_layout FROM sys.databases d WHERE {predicate} OPTION(MAXDOP 1)",
            current_id = crate::database_identity::catalog::CURRENT_DATABASE_ID,
            current_name = crate::database_identity::catalog::CURRENT_DATABASE_NAME,
            predicate = crate::database_identity::catalog::predicate("d")
        )
    };
    let available = select("d.is_cdc_enabled").replace('\'', "''");
    crate::database_identity::catalog::bind(&format!(
        "IF COL_LENGTH(N'sys.databases',N'is_cdc_enabled') IS NULL BEGIN {} END ELSE BEGIN EXEC sys.sp_executesql N'{available}',N'@P1 nvarchar(128),@culprit_catalog_current_id int,@culprit_catalog_current_name sysname',@P1=@P1,@culprit_catalog_current_id=@culprit_catalog_current_id,@culprit_catalog_current_name=@culprit_catalog_current_name; END",
        select("NULL")
    ))
}
pub(super) const SCANS: &str = r#"SELECT TOP (@P1) /* culprit */ DB_ID() AS database_id,
 CONVERT(varchar(40),SYSUTCDATETIME(),127)+'Z' AS observed_at,session_id,
 CONVERT(varchar(40),CAST(start_time AS datetime2),126) AS started,
 CONVERT(varchar(40),CAST(end_time AS datetime2),126) AS ended,scan_phase,duration,error_count,
 tran_count,log_record_count,command_count,failed_sessions_count,latency,
 CONVERT(varchar(40),CAST(last_commit_time AS datetime2),126) AS last_commit,
 CONVERT(varchar(40),CAST(last_commit_cdc_time AS datetime2),126) AS last_cdc_commit,
 start_lsn,current_lsn,end_lsn FROM sys.dm_cdc_log_scan_sessions OPTION(MAXDOP 1)"#;
pub(super) const ERRORS: &str = r#"SELECT TOP (@P1) /* culprit */ DB_ID() AS database_id,
 CONVERT(varchar(40),SYSUTCDATETIME(),127)+'Z' AS observed_at,session_id,phase_number,
 CONVERT(varchar(40),CAST(entry_time AS datetime2),126) AS entered,
 error_number,error_severity,error_state,start_lsn,begin_lsn,sequence_value
 FROM sys.dm_cdc_errors OPTION(MAXDOP 1)"#;
