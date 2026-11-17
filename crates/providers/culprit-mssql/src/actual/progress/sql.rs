//! Fixed, session/database-bounded native batches. Network reads stay layout/request/profiles; native work is
//! changed by the roster materialization and final guard and remains cost-unqualified.
use crate::activity::{
    page_server_reads::{self, Availability},
    participants::CHECK_COLUMNS,
};

pub(super) const MAX_NODES: usize = 4096;
const SCOPE_COLUMNS:&str="DB_ID() AS selected_database_id,DB_NAME() AS selected_database_name,@@SPID AS reader_session_id,
 CONVERT(varchar(40),CAST((SELECT login_time FROM sys.dm_exec_sessions WHERE session_id=@@SPID) AS datetime2),126) AS reader_session_generation,
 CONVERT(nvarchar(60),DATABASEPROPERTYEX(DB_NAME(),'Updateability')) AS database_updateability";

pub(super) fn request(live_xml: bool, availability: Availability) -> String {
    let live = if live_xml {
        // The DMF takes a session, but returns independent request/SQL/plan keys. A matching session alone is
        // insufficient under MARS. Do not enable profiling or accept another request's XML.
        "OUTER APPLY(SELECT sx.query_plan FROM sys.dm_exec_query_statistics_xml(r.session_id) sx
          WHERE sx.session_id=r.session_id AND sx.request_id=r.request_id
          AND sx.sql_handle=r.sql_handle AND sx.plan_handle=r.plan_handle) live"
    } else {
        ""
    };
    let live_column = if live_xml { "CONVERT(nvarchar(max),live.query_plan)" } else { "CAST(NULL AS nvarchar(max))" };
    format!(
        r#"
DECLARE /* culprit */ @at datetime2=SYSUTCDATETIME();
DECLARE @selected TABLE(session_id smallint,request_id int,session_generation datetime,start_time datetime,
 database_id smallint,sql_handle varbinary(64),plan_handle varbinary(64),statement_start_offset int,statement_end_offset int,
 query_hash binary(8),query_plan_hash binary(8),status nvarchar(30),command nvarchar(32),wait_type nvarchar(60),
 wait_resource nvarchar(256),wait_time bigint,total_elapsed_time bigint,cpu_time bigint,logical_reads bigint,percent_complete real,page_server_reads bigint);
INSERT INTO @selected
SELECT TOP(2) r.session_id,r.request_id,s.login_time,r.start_time,r.database_id,r.sql_handle,r.plan_handle,
 r.statement_start_offset,r.statement_end_offset,r.query_hash,r.query_plan_hash,r.status,r.command,r.wait_type,
 r.wait_resource,r.wait_time,r.total_elapsed_time,r.cpu_time,r.logical_reads,r.percent_complete,{page_server_reads}
FROM sys.dm_exec_requests r JOIN sys.dm_exec_sessions s ON s.session_id=r.session_id
WHERE r.session_id=@P1 AND r.database_id=DB_ID() AND r.session_id<>@@SPID AND s.is_user_process=1;
IF (SELECT COUNT(*) FROM @selected)<>1
BEGIN
 SELECT (SELECT COUNT(*) FROM @selected) AS request_count,CONVERT(varchar(40),@at,126)+'Z' AS observed_at;
 RETURN;
END;
SELECT 1 AS request_count,{CHECK_COLUMNS},{SCOPE_COLUMNS},{capability} AS request_page_server_read_capability,
 r.status,r.command,r.wait_type,r.wait_time,r.total_elapsed_time AS elapsed_ms,r.cpu_time AS cpu_ms,r.logical_reads,r.percent_complete,
 r.page_server_reads AS request_page_server_reads,{availability} AS request_page_server_read_availability,
 CONVERT(varchar(18),r.query_hash,1) AS query_hash,
 SUBSTRING(t.text,r.statement_start_offset/2+1,
  (CASE r.statement_end_offset WHEN -1 THEN DATALENGTH(t.text) ELSE r.statement_end_offset END-r.statement_start_offset)/2+1) AS statement_text,
 {live_column} AS live_plan,qp.query_plan AS cached_plan
FROM @selected r CROSS APPLY(SELECT 0 AS ordinal)a CROSS APPLY(SELECT r.session_generation AS login_time)s
OUTER APPLY sys.dm_exec_sql_text(r.sql_handle)t
{live}
OUTER APPLY sys.dm_exec_text_query_plan(r.plan_handle,r.statement_start_offset,r.statement_end_offset)qp
OPTION(MAXDOP 1);
"#,
        page_server_reads = page_server_reads::column(availability, "r"),
        availability = page_server_reads::current_availability(availability),
        capability = page_server_reads::CAPABILITY,
    )
}

pub(super) fn profiles() -> String {
    // The selected request's generation and locator constrain the operator aggregation. The final SELECT always
    // emits one guard, even if profiling is empty, so a vanished or recycled participant cannot bypass proof.
    format!(
        r#"
DECLARE /* culprit */ @at datetime2;
SELECT TOP({limit}) 'profile' AS row_kind,p.node_id,SUM(p.row_count) AS row_count,MAX(p.estimate_row_count) AS estimate_row_count,
 MAX(p.elapsed_time_ms) AS elapsed_ms,SUM(p.cpu_time_ms) AS cpu_ms,SUM(p.logical_read_count) AS logical_reads,
 SUM(p.end_of_scan_count) AS end_of_scans,SUM(p.rebind_count)+SUM(p.rewind_count) AS replays,
 MIN(CASE WHEN p.close_time>0 THEN 1 ELSE 0 END) AS closed
FROM sys.dm_exec_query_profiles p
JOIN sys.dm_exec_requests r ON r.session_id=p.session_id AND r.request_id=p.request_id
 AND r.sql_handle=p.sql_handle AND r.plan_handle=p.plan_handle
JOIN sys.dm_exec_sessions s ON s.session_id=r.session_id
WHERE r.session_id=@P1 AND r.request_id=@P2 AND r.database_id=DB_ID() AND r.database_id=@P9
 AND r.session_id<>@@SPID AND s.is_user_process=1
 AND s.login_time=CONVERT(datetime,@P3,126) AND r.start_time=CONVERT(datetime,@P4,126)
 AND r.sql_handle=CONVERT(varbinary(64),@P5,2) AND r.plan_handle=CONVERT(varbinary(64),@P6,2)
 AND r.statement_start_offset=@P7 AND r.statement_end_offset=@P8
GROUP BY p.node_id OPTION(MAXDOP 1);
SET @at=SYSUTCDATETIME();
SELECT 'guard' AS row_kind,{CHECK_COLUMNS},{SCOPE_COLUMNS},{capability} AS request_page_server_read_capability,
 (SELECT COUNT(*) FROM sys.dm_exec_requests ar WHERE ar.session_id=@P1 AND ar.database_id=DB_ID()) AS request_count
FROM(SELECT 0 AS ordinal)a
LEFT JOIN sys.dm_exec_sessions s ON s.session_id=@P1 AND s.is_user_process=1 AND s.session_id<>@@SPID
LEFT JOIN sys.dm_exec_requests r ON r.session_id=s.session_id AND r.request_id=@P2 AND r.database_id=DB_ID()
OPTION(MAXDOP 1);
"#,
        limit = MAX_NODES + 1,
        capability = page_server_reads::CAPABILITY,
    )
}
