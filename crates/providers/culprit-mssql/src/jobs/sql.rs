//! Fixed projections omit Agent commands, history messages, descriptions, credentials and output paths.
use super::*;
pub(super) fn jobs(activity: bool) -> String {
    let columns = if activity {
        r#"a.session_id AS activity_session_id,
 CONVERT(varchar(23),a.run_requested_date,126) AS requested_without_offset,
 CONVERT(nvarchar(128),a.run_requested_source) AS request_source,
 CONVERT(varchar(23),a.queued_date,126) AS queued_without_offset,
 CONVERT(varchar(23),a.start_execution_date,126) AS started_without_offset,
 CONVERT(varchar(23),a.stop_execution_date,126) AS stopped_without_offset,
 a.last_executed_step_id,
 CONVERT(varchar(23),a.last_executed_step_date,126) AS last_step_without_offset,
 CONVERT(varchar(23),a.next_scheduled_run_date,126) AS next_scheduled_without_offset"#
    } else {
        r#"CAST(NULL AS int) AS activity_session_id,
 CAST(NULL AS varchar(23)) AS requested_without_offset,CAST(NULL AS nvarchar(128)) AS request_source,
 CAST(NULL AS varchar(23)) AS queued_without_offset,CAST(NULL AS varchar(23)) AS started_without_offset,
 CAST(NULL AS varchar(23)) AS stopped_without_offset,CAST(NULL AS int) AS last_executed_step_id,
 CAST(NULL AS varchar(23)) AS last_step_without_offset,CAST(NULL AS varchar(23)) AS next_scheduled_without_offset"#
    };
    let join =
        if activity { "LEFT JOIN msdb.dbo.sysjobactivity a ON a.job_id=j.job_id AND a.session_id=@P2" } else { "" };
    format!(
        r#"SELECT TOP (@P1) /* culprit */ CONVERT(varchar(40),SYSUTCDATETIME(),127)+'Z' AS observed_at,
 j.job_id,j.name,j.enabled,j.start_step_id,j.version_number,
 CONVERT(varchar(23),j.date_created,126) AS created_without_offset,
 CONVERT(varchar(23),j.date_modified,126) AS modified_without_offset,{columns}
 FROM msdb.dbo.sysjobs j {join} ORDER BY j.job_id"#
    )
}
pub(super) const STEPS: &str = r#"SELECT TOP (@P1) /* culprit */
 CONVERT(varchar(40),SYSUTCDATETIME(),127)+'Z' AS observed_at,
 job_id,step_id,step_name,subsystem,database_name
 FROM msdb.dbo.sysjobsteps ORDER BY job_id,step_id"#;
pub(super) const HISTORY: &str = r#"SELECT TOP (@P1) /* culprit */
 CONVERT(varchar(40),SYSUTCDATETIME(),127)+'Z' AS observed_at,
 instance_id,job_id,step_id,step_name,run_status,run_date,run_time,run_duration,
 sql_message_id,sql_severity,retries_attempted,server
 FROM msdb.dbo.sysjobhistory ORDER BY instance_id DESC"#;
pub(super) fn context(metadata: &MssqlAgentJobsMetadata) -> String {
    let session = if metadata.available(MssqlAgentObject::Sessions) {
        r#"OUTER APPLY (SELECT TOP (1) session_id,
 CONVERT(varchar(23),agent_start_date,126) AS started_without_offset
 FROM msdb.dbo.syssessions ORDER BY session_id DESC) s"#
    } else {
        ""
    };
    let services = if metadata.available(MssqlAgentObject::Services) {
        r#"OUTER APPLY (
 SELECT TOP (3) servicename,status,CONVERT(varchar(48),last_startup_time,127) AS started_with_offset
 FROM sys.dm_server_services WHERE servicename LIKE N'SQL Server Agent (%' ORDER BY servicename) v"#
    } else {
        ""
    };
    let session_columns = if session.is_empty() {
        "CAST(NULL AS int) AS session_id,CAST(NULL AS varchar(23)) AS agent_started_without_offset"
    } else {
        "s.session_id,s.started_without_offset AS agent_started_without_offset"
    };
    let service_columns = if services.is_empty() {
        "CAST(NULL AS nvarchar(256)) AS servicename,CAST(NULL AS int) AS service_status,CAST(NULL AS varchar(48)) AS started_with_offset"
    } else {
        "v.servicename,v.status AS service_status,v.started_with_offset"
    };
    format!(
        "/* culprit */ DECLARE @observed_at datetime2(7)=SYSUTCDATETIME(); SELECT /* culprit */ CONVERT(varchar(40),@observed_at,127)+'Z' AS observed_at,{session_columns},{service_columns} FROM (VALUES(1)) base(only_row) {session} {services}"
    )
}
