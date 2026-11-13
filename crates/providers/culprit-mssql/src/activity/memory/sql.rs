use super::*;

pub(super) fn observation(layout: &fields::Layout, identity: IdentityDetail) -> String {
    let rows = format!(
        r#"SELECT {request},
  CASE WHEN r.request_id IS NULL THEN 1 ELSE 0 END AS no_current_request,
  CASE WHEN r.database_id=DB_ID() THEN 1 ELSE 5 END AS scope_basis,
  CAST(NULL AS bigint) AS transaction_id,CAST(NULL AS varchar(40)) AS transaction_start,
  mg.session_id AS grant_session_id,mg.request_id AS grant_request_id,
  CONVERT(varchar(40),CAST(mg.request_time AS datetime2),126) AS grant_requested_without_offset,
  CONVERT(varchar(40),CAST(mg.grant_time AS datetime2),126) AS grant_granted_without_offset,
  CONVERT(varchar(128),mg.sql_handle,2) AS grant_sql_handle,
  CONVERT(varchar(128),mg.plan_handle,2) AS grant_plan_handle,
  CONVERT(varchar(128),r.plan_handle,2) AS request_plan_handle,
  mg.pool_id AS grant_pool_id,mg.group_id AS grant_group_id,mg.resource_semaphore_id AS grant_semaphore_id,
  {counters}
 FROM sys.dm_exec_query_memory_grants mg
 LEFT JOIN sys.dm_exec_requests r ON r.session_id=mg.session_id AND r.request_id=mg.request_id
 LEFT JOIN sys.dm_exec_sessions s ON s.session_id=mg.session_id
 LEFT JOIN sys.dm_exec_sessions b ON b.session_id=r.blocking_session_id
 WHERE mg.session_id>0 AND mg.session_id<>@@SPID
  AND (s.is_user_process=1 OR s.session_id IS NULL)
  AND (r.database_id=DB_ID() OR NULLIF(r.database_id,0) IS NULL)"#,
        request = super::super::sql::REQUEST_COLUMNS,
        counters = layout.projection(),
    );
    // No ranking, grouping, plans or full text in the current-grant probe. Those could add a memory grant to the
    // diagnostic itself. The row cap still does not bound all work of native DMV/session/request joins.
    super::super::sql::frame_query(&rows, identity, super::super::sql::QueryPolicy::MemoryDiagnostic)
}

pub(super) fn context(identity: IdentityDetail) -> String {
    super::super::sql::frame_query(
        "SELECT CAST(NULL AS int) AS session_id WHERE 1=0",
        identity,
        super::super::sql::QueryPolicy::MemoryDiagnostic,
    )
}
