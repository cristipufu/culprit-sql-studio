use super::super::{
    IdentityDetail,
    sql::{QueryPolicy, REQUEST_COLUMNS, frame_query},
};

pub(super) fn context(identity: IdentityDetail) -> String {
    frame_query(
        r#"SELECT CAST(NULL AS int) AS session_id,
 CASE WHEN CAST(SERVERPROPERTY('EngineEdition') AS int)=5 THEN
  CASE WHEN IS_SRVROLEMEMBER('##MS_ServerStateReader##')=1
    OR HAS_PERMS_BY_NAME(NULL,NULL,'VIEW SERVER STATE')=1
    OR HAS_PERMS_BY_NAME(NULL,NULL,'CONTROL SERVER')=1 THEN 1
   WHEN NULLIF(CONVERT(nvarchar(32),DATABASEPROPERTYEX(DB_NAME(),'ServiceObjective')),'') IS NOT NULL
    AND CONVERT(nvarchar(32),DATABASEPROPERTYEX(DB_NAME(),'ServiceObjective')) NOT IN('Basic','S0','S1','ElasticPool','System')
    AND HAS_PERMS_BY_NAME(DB_NAME(),'DATABASE','VIEW DATABASE STATE')=1 THEN 1 ELSE 0 END
 WHEN CAST(PARSENAME(CONVERT(varchar(128),SERVERPROPERTY('ProductVersion')),4) AS int)>=16
  THEN CASE WHEN HAS_PERMS_BY_NAME(NULL,NULL,'VIEW SERVER PERFORMANCE STATE')=1 THEN 1 ELSE 0 END
 ELSE CASE WHEN HAS_PERMS_BY_NAME(NULL,NULL,'VIEW SERVER STATE')=1 THEN 1 ELSE 0 END END AS task_space_permission"#,
        identity,
        QueryPolicy::SerialMetadata,
    )
}

pub(super) fn observation(identity: IdentityDetail) -> String {
    let rows = format!(
        r#"SELECT {REQUEST_COLUMNS},
 CASE WHEN r.request_id IS NULL THEN 1 ELSE 0 END AS no_current_request,
 CASE WHEN r.database_id=DB_ID() THEN 1 ELSE 5 END AS scope_basis,
 CAST(NULL AS bigint) AS transaction_id,CAST(NULL AS varchar(40)) AS transaction_start,
 su.session_id AS task_session_id,su.request_id AS task_request_id,su.exec_context_id AS task_execution_context_id,
 su.database_id AS task_database_id,
 su.user_objects_alloc_page_count AS user_allocated_pages,su.user_objects_dealloc_page_count AS user_deallocated_pages,
 su.internal_objects_alloc_page_count AS internal_allocated_pages,su.internal_objects_dealloc_page_count AS internal_deallocated_pages
 FROM sys.dm_db_task_space_usage su
 LEFT JOIN sys.dm_exec_requests r ON r.session_id=su.session_id AND r.request_id=su.request_id
 LEFT JOIN sys.dm_exec_sessions s ON s.session_id=su.session_id
 LEFT JOIN sys.dm_exec_sessions b ON b.session_id=r.blocking_session_id
 WHERE su.session_id>0 AND su.session_id<>@@SPID
  AND (s.is_user_process=1 OR s.session_id IS NULL)
  AND (r.database_id=DB_ID() OR NULLIF(r.database_id,0) IS NULL)"#
    );
    // Unsorted native task counters only: no session totals, net sums, SQL text, plans or task-address joins.
    // TOP limits returned records; native DMV traversal/join cost remains subject to separate qualification.
    frame_query(&rows, identity, QueryPolicy::SerialMetadata)
}
