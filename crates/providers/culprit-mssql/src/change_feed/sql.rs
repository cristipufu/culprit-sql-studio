//! Published native catalog columns only. TOP bounds returned rows, not server-internal work.
use crate::change_capture::layout;
// https://learn.microsoft.com/sql/relational-databases/system-dynamic-management-objects/sys-dm-change-feed-log-scan-sessions
pub(super) const SCAN_COLUMNS: &[&str] = &[
    "session_id",
    "start_time",
    "end_time",
    "duration",
    "batch_processing_phase",
    "error_count",
    "batch_start_lsn",
    "currently_processed_lsn",
    "batch_end_lsn",
    "tran_count",
    "currently_processed_commit_lsn",
    "currently_processed_commit_time",
    "log_record_count",
    "schema_change_count",
    "command_count",
    "latency",
    "empty_scan_count",
    "failed_sessions_count",
];
// error_message is deliberately neither selected nor included in the required layout.
pub(super) const ERROR_COLUMNS: &[&str] = &[
    "session_id",
    "source_task",
    "table_id",
    "table_group_id",
    "capture_phase_number",
    "entry_time",
    "error_number",
    "error_severity",
    "error_state",
    "batch_start_lsn",
    "batch_end_lsn",
    "tran_begin_lsn",
    "tran_commit_lsn",
    "sequence_value",
    "command_id",
];
pub(super) fn availability() -> String {
    let scans = layout("sys.dm_change_feed_log_scan_sessions", SCAN_COLUMNS);
    let errors = layout("sys.dm_change_feed_errors", ERROR_COLUMNS);
    format!(
        "SELECT /* culprit */ DB_ID() AS database_id,CONVERT(varchar(40),SYSUTCDATETIME(),127)+'Z' AS observed_at,HAS_PERMS_BY_NAME(DB_NAME(),'DATABASE',N'VIEW DATABASE STATE') AS has_database_state,HAS_PERMS_BY_NAME(DB_NAME(),'DATABASE',N'VIEW DATABASE PERFORMANCE STATE') AS has_database_performance_state,{scans} AS scan_layout,{errors} AS error_layout OPTION(MAXDOP 1)"
    )
}
pub(super) const SCANS: &str = r#"SELECT TOP (@P1) /* culprit */ DB_ID() AS database_id,
 CONVERT(varchar(40),SYSUTCDATETIME(),127)+'Z' AS observed_at,session_id,
 CONVERT(varchar(40),CAST(start_time AS datetime2),126) AS started,
 CONVERT(varchar(40),CAST(end_time AS datetime2),126) AS ended,duration,batch_processing_phase,error_count,
 batch_start_lsn,currently_processed_lsn,batch_end_lsn,tran_count,currently_processed_commit_lsn,
 CONVERT(varchar(40),CAST(currently_processed_commit_time AS datetime2),126) AS processed_commit,
 log_record_count,schema_change_count,command_count,latency,empty_scan_count,failed_sessions_count
 FROM sys.dm_change_feed_log_scan_sessions OPTION(MAXDOP 1)"#;
pub(super) const ERRORS: &str = r#"SELECT TOP (@P1) /* culprit */ DB_ID() AS database_id,
 CONVERT(varchar(40),SYSUTCDATETIME(),127)+'Z' AS observed_at,session_id,source_task,table_id,
 CONVERT(varchar(36),table_group_id) AS table_group_id,capture_phase_number,
 CONVERT(varchar(40),CAST(entry_time AS datetime2),126) AS entered,
 error_number,error_severity,error_state,batch_start_lsn,batch_end_lsn,tran_begin_lsn,tran_commit_lsn,sequence_value,command_id
 FROM sys.dm_change_feed_errors OPTION(MAXDOP 1)"#;
