//! Targeted header/catalog reads only. The provided XML contains at most the admitted native page addresses.
//! Page-header access can itself read or latch pages: the small input set is not a latency/physical-I/O guarantee.
use super::{super::IdentityDetail, fields};

/// Only active user requests already reporting page waits are sampled. Reuse the shared request columns/decoder,
/// without idle-transaction probes, SQL text, plans, grouping or ordering in this opt-in diagnostic.
pub(super) fn observation(identity: IdentityDetail) -> String {
    let requests = format!(
        r#"SELECT {request},
 CASE WHEN r.request_id IS NULL THEN 1 ELSE 0 END AS no_current_request,
 CASE WHEN r.database_id=DB_ID() THEN 1 ELSE 5 END AS scope_basis,
 CAST(NULL AS bigint) AS transaction_id,CAST(NULL AS varchar(40)) AS transaction_start,
 {task}
 FROM sys.dm_exec_requests r
 JOIN sys.dm_exec_sessions s ON s.session_id=r.session_id
 LEFT JOIN sys.dm_exec_sessions b ON b.session_id=r.blocking_session_id
 WHERE r.session_id<>@@SPID AND s.is_user_process=1 AND r.database_id=DB_ID()
  AND (r.wait_type LIKE 'PAGELATCH[_]%' OR r.wait_type LIKE 'PAGEIOLATCH[_]%')"#,
        request = super::super::sql::REQUEST_COLUMNS,
        task = super::task_waits::empty_projection(),
    );
    // The DMV task/wait populations can still require enumeration. TOP bounds the returned artifacts only;
    // neither LOOP JOIN nor address equality is a proven native seek/cost bound. This entire explicit profile
    // stays CostUnqualified pending actual-plan/load/cancellation qualification of the extended projection.
    // waiting_tasks has no request_id: dm_os_tasks supplies that native association, including MARS requests.
    // https://learn.microsoft.com/sql/relational-databases/system-dynamic-management-objects/sys-dm-os-tasks-transact-sql
    let tasks = format!(
        r#"SELECT {request},0 AS no_current_request,1 AS scope_basis,
 CAST(NULL AS bigint) AS transaction_id,CAST(NULL AS varchar(40)) AS transaction_start,
 {task}
 FROM sys.dm_os_waiting_tasks w
 JOIN sys.dm_os_tasks task ON task.task_address=w.waiting_task_address
  AND task.session_id=w.session_id AND task.exec_context_id=w.exec_context_id
 JOIN sys.dm_exec_requests r ON r.session_id=task.session_id AND r.request_id=task.request_id
 JOIN sys.dm_exec_sessions s ON s.session_id=r.session_id
 LEFT JOIN sys.dm_exec_sessions b ON b.session_id=r.blocking_session_id
 WHERE r.session_id<>@@SPID AND s.is_user_process=1 AND r.database_id=DB_ID()
  AND (w.wait_type LIKE 'PAGELATCH[_]%' OR w.wait_type LIKE 'PAGEIOLATCH[_]%')"#,
        request = super::super::sql::REQUEST_COLUMNS,
        task = super::task_waits::projection(),
    );
    let rows = format!("{requests}\nUNION ALL\n{tasks}");
    let select = |rows: &str| {
        super::super::sql::frame_select_with_context(
            rows,
            identity,
            super::super::sql::QueryPolicy::SerialMetadata,
            "@task_visibility AS task_visibility,",
        )
    };
    // Azure's role-specific documentation demonstrates this membership function in user databases, while
    // the general function documentation says it can return 0 there. Only a positive result grants admission;
    // 0/NULL never establish task absence. No virtual-master, service-objective or owner-name probe is made.
    // https://learn.microsoft.com/azure/azure-sql/database/security-server-roles#c-complete-example-add-a-login-to-a-server-level-role-retrieve-metadata-for-role-membership-and-permissions-and-run-a-test-query
    let permission = r#"CASE WHEN CAST(SERVERPROPERTY('EngineEdition') AS int)=5 THEN
 CASE WHEN DB_NAME()<>'master' AND IS_SRVROLEMEMBER('##MS_ServerStateReader##')=1
  AND HAS_PERMS_BY_NAME(DB_NAME(),'DATABASE','VIEW DATABASE STATE')=1 THEN 1 ELSE 0 END
 ELSE CASE WHEN CAST(PARSENAME(CONVERT(varchar(128),SERVERPROPERTY('ProductVersion')),4) AS int)>=16
  THEN CASE WHEN HAS_PERMS_BY_NAME(NULL,NULL,'VIEW SERVER PERFORMANCE STATE')=1 THEN 1 ELSE 0 END
  ELSE CASE WHEN HAS_PERMS_BY_NAME(NULL,NULL,'VIEW SERVER STATE')=1 THEN 1 ELSE 0 END END END"#;
    format!(
        "{clock}DECLARE /* culprit */ @task_visibility bit={permission};\nIF @task_visibility=1\nBEGIN\n{tasks}\nEND\nELSE\nBEGIN\n{requests}\nEND",
        clock = super::super::sql::FRAME_CLOCK,
        tasks = select(&rows),
        requests = select(&requests),
    )
}

/// A durable focus admits only its original native request. It never selects a replacement population.
pub(in crate::activity) fn focused_observation(identity: IdentityDetail) -> String {
    let rows = format!(
        r#"SELECT {request},
 CASE WHEN r.request_id IS NULL THEN 1 ELSE 0 END AS no_current_request,
 CASE WHEN r.database_id=DB_ID() THEN 1 ELSE 5 END AS scope_basis,
 CAST(NULL AS bigint) AS transaction_id,CAST(NULL AS varchar(40)) AS transaction_start
 FROM sys.dm_exec_requests r
 JOIN sys.dm_exec_sessions s ON s.session_id=r.session_id
 LEFT JOIN sys.dm_exec_sessions b ON b.session_id=r.blocking_session_id
 WHERE r.session_id=@P2 AND r.request_id=@P3 AND r.session_id<>@@SPID AND s.is_user_process=1"#,
        request = super::super::sql::REQUEST_COLUMNS
    );
    super::super::sql::frame_query(&rows, identity, super::super::sql::QueryPolicy::SerialMetadata)
}

pub(super) fn headers() -> String {
    headers_in("DB_ID()", "sys", "a.database_id=DB_ID()")
}

pub(super) fn shared_tempdb_headers() -> String {
    headers_in("2", "tempdb.sys", "a.database_id=2 AND DB_ID(N'tempdb')=2")
}

pub(super) fn headers_with_shared_tempdb() -> String {
    // Distinct XML populations fence the function arguments before relational optimization. WHERE alone
    // cannot guarantee that a function will never see a foreign page from the other branch.
    let request = header_select("DB_ID()", "sys", "a.database_id=DB_ID()", "/pages/request_database/page");
    let shared =
        header_select("2", "tempdb.sys", "a.database_id=2 AND DB_ID(N'tempdb')=2", "/pages/shared_tempdb/page");
    format!("{}\n{request}\nUNION ALL\n{shared}\nOPTION(MAXDOP 1,LOOP JOIN)", header_clock())
}

fn headers_in(database: &str, catalog: &str, guard: &str) -> String {
    format!(
        "{}\n{}\nOPTION(MAXDOP 1,LOOP JOIN)",
        header_clock(),
        header_select(database, catalog, guard, "/pages/page")
    )
}

fn header_clock() -> &'static str {
    "DECLARE /* culprit */ @targets xml=CAST(@P1 AS xml),@at datetime2=SYSUTCDATETIME();"
}

fn header_select(database: &str, catalog: &str, guard: &str, path: &str) -> String {
    format!(
        r#"
SELECT /* culprit */
 a.ordinal,a.database_id AS target_database_id,a.file_id AS target_file_id,a.page_id AS target_page_id,
 CONVERT(varchar(40),@at,126)+'Z' AS observed_at,
 {header},
 CONVERT(varchar(36),f.file_guid) AS file_guid,CONVERT(varchar(30),f.create_lsn) AS file_create_lsn,
 f.type AS file_type,f.state AS file_state,
 o.object_id AS catalog_object_id,s.name AS schema_name,o.name AS object_name,
 CONVERT(varchar(40),CAST(o.create_date AS datetime2),126) AS object_created_without_offset,
 CONVERT(varchar(40),CAST(o.modify_date AS datetime2),126) AS object_modified_without_offset,
 i.index_id AS catalog_index_id,i.name AS index_name,i.type AS index_type,
 part.partition_id AS catalog_partition_id,part.hobt_id AS catalog_hobt_id,
 part.partition_number AS catalog_partition_number
FROM @targets.nodes('{path}') t(n)
CROSS APPLY(SELECT t.n.value('@ordinal[1]','int') AS ordinal,t.n.value('@database[1]','int') AS database_id,
 t.n.value('@file[1]','int') AS file_id,t.n.value('@page[1]','int') AS page_id) a
OUTER APPLY sys.dm_db_page_info({database},a.file_id,a.page_id,'LIMITED') p
LEFT JOIN {catalog}.database_files f ON f.file_id=a.file_id
LEFT JOIN {catalog}.all_objects o ON o.object_id=p.object_id
LEFT JOIN {catalog}.schemas s ON s.schema_id=o.schema_id
LEFT JOIN {catalog}.indexes i ON i.object_id=p.object_id AND i.index_id=p.index_id
LEFT JOIN {catalog}.partitions part ON part.object_id=p.object_id AND part.index_id=p.index_id AND part.partition_id=p.partition_id
WHERE {guard}"#,
        header = fields::projection()
    )
}
