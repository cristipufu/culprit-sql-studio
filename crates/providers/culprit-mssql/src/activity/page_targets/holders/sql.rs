use crate::activity::{
    IdentityDetail,
    sql::{self as activity, QueryPolicy},
};

/// Only the original task slot and its natively reported blocking-task pointer are considered. Native DMVs
/// need not seek these predicates; this separately admitted profile remains cost-Unqualified.
pub(super) fn point(identity: IdentityDetail) -> String {
    let task = super::super::task_waits::gated_projection("w.wait_type LIKE 'PAGELATCH[_]%'");
    let rows = actors(&format!(
        r#"OUTER APPLY(
 SELECT TOP (2) {task},
  CONVERT(varchar(16),task.task_address,2) AS checked_task_address,
  task.session_id AS checked_task_session_id,task.request_id AS checked_task_request_id,
  task.exec_context_id AS checked_task_execution_context_id,
  w.wait_type AS checked_task_wait_type,w.resource_description AS checked_task_resource,
  w.blocking_task_address AS reported_address,w.blocking_session_id AS reported_session,
  w.blocking_exec_context_id AS reported_context
 FROM sys.dm_os_tasks task
 LEFT JOIN sys.dm_os_waiting_tasks w ON w.waiting_task_address=task.task_address
  AND w.session_id=task.session_id AND w.exec_context_id=task.exec_context_id
 WHERE task.task_address=CONVERT(varbinary(8),@P4,2)
  AND task.session_id=@P2 AND task.request_id=@P3 AND task.exec_context_id=@P5
) selected
OUTER APPLY(
 SELECT TOP (2) holder.task_address,holder.session_id,holder.request_id,holder.exec_context_id
 FROM sys.dm_os_tasks holder
 WHERE holder.task_address=CASE WHEN selected.reported_session>0
  AND selected.reported_address<>CONVERT(varbinary(8),@P4,2) THEN selected.reported_address END
) holder"#,
    ));
    let unavailable = actors(&format!(
        r#"OUTER APPLY(SELECT {empty},
 CAST(NULL AS varchar(16)) AS checked_task_address,CAST(NULL AS smallint) AS checked_task_session_id,
 CAST(NULL AS int) AS checked_task_request_id,CAST(NULL AS int) AS checked_task_execution_context_id,
 CAST(NULL AS nvarchar(60)) AS checked_task_wait_type,CAST(NULL AS nvarchar(3072)) AS checked_task_resource) selected
OUTER APPLY(SELECT CAST(NULL AS varbinary(8)) AS task_address,CAST(NULL AS smallint) AS session_id,
 CAST(NULL AS int) AS request_id,CAST(NULL AS int) AS exec_context_id) holder"#,
        empty = super::super::task_waits::empty_projection(),
    ));
    let select = |rows: &str| {
        activity::frame_select_with_context(
            rows,
            identity,
            QueryPolicy::SerialMetadata,
            "@task_visibility AS task_visibility,",
        )
    };
    format!(
        "{clock}DECLARE /* culprit */ @task_visibility bit={permission};\nIF @task_visibility=1\nBEGIN\n{visible}\nEND\nELSE\nBEGIN\n{unavailable}\nEND",
        clock = activity::FRAME_CLOCK,
        permission = super::super::sql::task_visibility(),
        visible = select(&rows),
        unavailable = select(&unavailable),
    )
}

fn actors(native_task: &str) -> String {
    format!(
        r#"SELECT {request},
 CASE WHEN r.request_id IS NULL THEN 1 ELSE 0 END AS no_current_request,
 CASE WHEN r.database_id=DB_ID() THEN 1 ELSE 5 END AS scope_basis,
 CAST(NULL AS bigint) AS transaction_id,CAST(NULL AS varchar(40)) AS transaction_start,
 {checks},selected.*,
 CONVERT(varchar(16),holder.task_address,2) AS holder_task_address,
 holder.session_id AS holder_session_id,holder.request_id AS holder_request_id,
 holder.exec_context_id AS holder_execution_context_id,s.is_user_process AS actor_user_process
FROM (SELECT 1 AS anchor) anchor
{native_task}
CROSS APPLY(SELECT 0 AS ordinal,@P2 AS session_id,@P3 AS request_id
 UNION ALL SELECT 1,holder.session_id,holder.request_id WHERE holder.task_address IS NOT NULL) a
LEFT JOIN sys.dm_exec_sessions s ON s.session_id=a.session_id
LEFT JOIN sys.dm_exec_requests r ON r.session_id=a.session_id AND r.request_id=a.request_id
LEFT JOIN sys.dm_exec_sessions b ON b.session_id=r.blocking_session_id"#,
        request = activity::REQUEST_COLUMNS,
        checks = crate::activity::participants::CHECK_COLUMNS,
    )
}
