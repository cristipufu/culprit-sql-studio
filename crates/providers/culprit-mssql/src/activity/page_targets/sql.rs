//! Targeted header/catalog reads only. The provided XML contains at most the admitted native page addresses.
//! Page-header access can itself read or latch pages: the small input set is not a latency/physical-I/O guarantee.
use super::{super::IdentityDetail, fields};

/// Only active user requests already reporting page waits are sampled. Reuse the shared request columns/decoder,
/// without idle-transaction probes, SQL text, plans, grouping or ordering in this opt-in diagnostic.
pub(super) fn observation(identity: IdentityDetail) -> String {
    let rows = format!(
        r#"SELECT {request},
 CASE WHEN r.request_id IS NULL THEN 1 ELSE 0 END AS no_current_request,
 CASE WHEN r.database_id=DB_ID() THEN 1 ELSE 5 END AS scope_basis,
 CAST(NULL AS bigint) AS transaction_id,CAST(NULL AS varchar(40)) AS transaction_start
 FROM sys.dm_exec_requests r
 JOIN sys.dm_exec_sessions s ON s.session_id=r.session_id
 LEFT JOIN sys.dm_exec_sessions b ON b.session_id=r.blocking_session_id
 WHERE r.session_id<>@@SPID AND s.is_user_process=1 AND r.database_id=DB_ID()
  AND (r.wait_type LIKE 'PAGELATCH[_]%' OR r.wait_type LIKE 'PAGEIOLATCH[_]%')"#,
        request = super::super::sql::REQUEST_COLUMNS
    );
    super::super::sql::frame_query(&rows, identity, super::super::sql::QueryPolicy::SerialMetadata)
}

pub(super) fn context(identity: IdentityDetail) -> String {
    super::super::sql::frame_query(
        r#"SELECT CAST(NULL AS int) AS session_id,
 sd.database_id AS selected_database_id,CONVERT(varchar(40),CAST(sd.create_date AS datetime2),126) AS database_created_without_offset,
 CONVERT(varchar(36),df.file_guid) AS database_file_guid,CONVERT(varchar(30),df.create_lsn) AS database_file_create_lsn,
 CASE WHEN OBJECT_ID('sys.dm_db_page_info') IS NOT NULL THEN 1 ELSE 0 END AS page_info_available,
 CASE WHEN HAS_PERMS_BY_NAME(DB_NAME(),'DATABASE','VIEW DATABASE STATE')=1
   OR HAS_PERMS_BY_NAME(DB_NAME(),'DATABASE','VIEW DATABASE PERFORMANCE STATE')=1 THEN 1 ELSE 0 END AS page_permission
 FROM sys.databases sd LEFT JOIN sys.database_files df ON df.file_id=1 AND df.type=0
 WHERE sd.database_id=DB_ID()"#,
        identity,
        super::super::sql::QueryPolicy::SerialMetadata,
    )
}

pub(super) fn headers() -> String {
    format!(
        r#"
DECLARE /* culprit */ @targets xml=CAST(@P1 AS xml),@at datetime2=SYSUTCDATETIME();
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
FROM @targets.nodes('/pages/page') t(n)
CROSS APPLY(SELECT t.n.value('@ordinal[1]','int') AS ordinal,t.n.value('@database[1]','int') AS database_id,
 t.n.value('@file[1]','int') AS file_id,t.n.value('@page[1]','int') AS page_id) a
OUTER APPLY sys.dm_db_page_info(DB_ID(),a.file_id,a.page_id,'LIMITED') p
LEFT JOIN sys.database_files f ON f.file_id=a.file_id
LEFT JOIN sys.all_objects o ON o.object_id=p.object_id
LEFT JOIN sys.schemas s ON s.schema_id=o.schema_id
LEFT JOIN sys.indexes i ON i.object_id=p.object_id AND i.index_id=p.index_id
LEFT JOIN sys.partitions part ON part.object_id=p.object_id AND part.index_id=p.index_id AND part.partition_id=p.partition_id
WHERE a.database_id=DB_ID()
OPTION(MAXDOP 1,LOOP JOIN)"#,
        header = fields::projection()
    )
}
