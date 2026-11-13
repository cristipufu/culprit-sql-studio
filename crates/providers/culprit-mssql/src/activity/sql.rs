//! Native activity projections. Scope and optional database identity are requested facts, independent of consumers.
use super::IdentityDetail;

#[derive(Clone, Copy)]
pub(super) enum QueryPolicy {
    Activity,
    /// This diagnostic must not request execution workspace memory while investigating grant pressure.
    MemoryDiagnostic,
}
impl QueryPolicy {
    pub fn hints(self) -> &'static str {
        match self {
            Self::Activity => "OPTION(MAXDOP 1)",
            Self::MemoryDiagnostic => "OPTION(MAXDOP 1, LOOP JOIN)",
        }
    }
}

/// Context and visibility are identical for the ordinary activity and explicit request-resource profiles.
pub(super) fn frame_query(rows: &str, identity: IdentityDetail, policy: QueryPolicy) -> String {
    let (identity_columns, identity_join) = match identity {
        IdentityDetail::Endpoint => {
            ("CAST(NULL AS varchar(36)) AS incarnation,CAST(NULL AS varchar(36)) AS physical_incarnation", "")
        }
        IdentityDetail::AzureDatabase => (
            "CONVERT(varchar(36),g.logical_database_guid) AS incarnation,CONVERT(varchar(36),g.physical_database_guid) AS physical_incarnation",
            "JOIN sys.dm_user_db_resource_governance g ON g.database_id=DB_ID() AND g.database_name=d.database_name",
        ),
    };
    let ordering = if matches!(policy, QueryPolicy::Activity) { "ORDER BY a.session_id,a.request_id" } else { "" };
    let hints = policy.hints();
    format!(
        r#"
DECLARE /* culprit */ @at datetime2=SYSUTCDATETIME(), @native_now datetime2=SYSDATETIME();
SELECT TOP (@P1) /* culprit */
 CAST(SERVERPROPERTY('EngineEdition') AS int) AS edition,
 CAST(DATABASEPROPERTYEX(DB_NAME(),'Updateability') AS nvarchar(32)) AS updateability,
 CONVERT(varchar(128),SERVERPROPERTY('ProductVersion')) AS server_version,
 (SELECT compatibility_level FROM sys.databases WHERE database_id=DB_ID()) AS compatibility_level,
 CAST(PARSENAME(CONVERT(varchar(128),SERVERPROPERTY('ProductVersion')),4) AS int) AS major,
 SUSER_SNAME() AS reader_login,USER_NAME() AS reader_user,
 CASE WHEN (CAST(SERVERPROPERTY('EngineEdition') AS int)=5 AND
   (HAS_PERMS_BY_NAME(DB_NAME(),'DATABASE','VIEW DATABASE STATE')=1 OR IS_SRVROLEMEMBER('##MS_ServerStateReader##')=1))
  OR HAS_PERMS_BY_NAME(NULL,NULL,'VIEW SERVER STATE')=1
  OR HAS_PERMS_BY_NAME(NULL,NULL,'VIEW SERVER PERFORMANCE STATE')=1 THEN 1 ELSE 0 END AS database_wide,
 d.database_name, CONVERT(varchar(40),@at,126)+'Z' AS observed_at,
 CONVERT(varchar(40),@native_now,126) AS native_observed_at,
 {identity_columns}, a.*
FROM (SELECT DB_NAME() AS database_name) d
{identity_join}
OUTER APPLY ({rows}) a
{ordering} {hints}"#
    )
}

/// One request projection and decoder own generations, waits, application context and guarded SQL locators.
pub(super) const REQUEST_COLUMNS: &str = r#"s.session_id,CONVERT(varchar(40),CAST(s.login_time AS datetime2),126) AS session_generation,
  r.request_id,CONVERT(varchar(40),CAST(r.start_time AS datetime2),126) AS request_generation,
  DB_NAME(COALESCE(r.database_id,s.database_id)) AS session_database,r.command,COALESCE(r.status,s.status) AS state,
  s.program_name,s.login_name,s.host_name,s.open_transaction_count,
  r.total_elapsed_time AS elapsed_ms,
  CONVERT(varchar(40),CAST(s.last_request_start_time AS datetime2),126) AS last_request_start,
  CONVERT(varchar(40),CAST(s.last_request_end_time AS datetime2),126) AS last_request_end,
  s.cpu_time AS session_cpu_ms,s.logical_reads AS session_logical_reads,s.reads AS session_physical_reads,s.writes AS session_writes,
  r.cpu_time AS request_cpu_ms,r.logical_reads AS request_logical_reads,r.reads AS request_physical_reads,r.writes AS request_writes,
  r.wait_type,r.wait_resource,CASE WHEN r.wait_type IS NOT NULL THEN r.wait_time END AS wait_ms,r.blocking_session_id,
  CASE WHEN r.blocking_session_id>0 THEN CONVERT(varchar(40),CAST(b.login_time AS datetime2),126) END AS blocker_generation,
  CONVERT(varchar(16),r.query_hash,2) AS query_hash,CONVERT(varchar(128),r.sql_handle,2) AS sql_handle,
  r.statement_start_offset AS statement_start,r.statement_end_offset AS statement_end,r.database_id AS request_database_id"#;

pub(super) fn metadata(identity: IdentityDetail) -> String {
    let rows = format!(
        r#"SELECT {REQUEST_COLUMNS},
  CASE WHEN NOT EXISTS(SELECT 1 FROM sys.dm_exec_requests ar WHERE ar.session_id=s.session_id) THEN 1 ELSE 0 END AS no_current_request,
  CASE WHEN r.request_id IS NOT NULL THEN CASE WHEN r.database_id=DB_ID() THEN 1
   WHEN s.session_id IN(SELECT blocking_session_id FROM sys.dm_exec_requests WHERE database_id=DB_ID() AND blocking_session_id>0) THEN 4 ELSE 5 END
   WHEN tx.database_id=DB_ID() THEN 2 WHEN s.database_id=DB_ID() THEN 3
   WHEN s.session_id IN(SELECT blocking_session_id FROM sys.dm_exec_requests WHERE database_id=DB_ID() AND blocking_session_id>0) THEN 4 ELSE 5 END AS scope_basis,
  tx.transaction_id,CONVERT(varchar(40),CAST(tx.transaction_start AS datetime2),126) AS transaction_start
 FROM sys.dm_exec_sessions s
 LEFT JOIN sys.dm_exec_requests r ON r.session_id=s.session_id AND (@P2=0 OR r.database_id=DB_ID())
 LEFT JOIN sys.dm_exec_sessions b ON b.session_id=r.blocking_session_id
 OUTER APPLY (
  SELECT TOP (1) st.transaction_id,at.transaction_begin_time AS transaction_start,dt.database_id
  FROM sys.dm_tran_session_transactions st
  JOIN sys.dm_tran_database_transactions dt ON dt.transaction_id=st.transaction_id AND (@P2=0 OR dt.database_id=DB_ID())
  JOIN sys.dm_tran_active_transactions at ON at.transaction_id=st.transaction_id
  WHERE st.session_id=s.session_id ORDER BY at.transaction_begin_time,st.transaction_id,dt.database_id
 ) tx
 WHERE s.is_user_process=1 AND s.session_id<>@@SPID
  AND (r.request_id IS NOT NULL OR s.open_transaction_count>0
   OR s.session_id IN(SELECT blocking_session_id FROM sys.dm_exec_requests WHERE blocking_session_id>0))
  AND (@P2=0 OR r.request_id IS NOT NULL OR (s.open_transaction_count>0 AND(s.database_id=DB_ID() OR tx.transaction_id IS NOT NULL))
   OR s.session_id IN(SELECT blocking_session_id FROM sys.dm_exec_requests WHERE database_id=DB_ID() AND blocking_session_id>0))
"#
    );
    frame_query(&rows, identity, QueryPolicy::Activity)
}

// XML selectors work on databases whose compatibility level predates OPENJSON. Values are typed and validated.
const CURRENT_SQL_BODY: &str = r#"
DECLARE /* culprit */ @ids xml=CONVERT(xml,@P1);
SELECT /* culprit */ i.session_id,i.session_generation,i.request_id,i.request_generation,
 CASE WHEN r.request_id IS NOT NULL AND r.sql_handle=CONVERT(varbinary(64),i.sql_handle,2)
  AND r.statement_start_offset=i.statement_start AND r.statement_end_offset=i.statement_end THEN 1 ELSE 0 END AS matched,
 t.encrypted,fragment.source_bytes,
 CASE WHEN fragment.source_bytes<=@P2 THEN SUBSTRING(t.text COLLATE Latin1_General_100_BIN2,r.statement_start_offset/2+1,fragment.source_bytes/2) END AS payload
FROM @ids.nodes('/activity/row') n(x)
CROSS APPLY(SELECT x.value('@session[1]','int') AS session_id,x.value('@login[1]','varchar(40)') AS session_generation,
 x.value('@request[1]','int') AS request_id,x.value('@started[1]','varchar(40)') AS request_generation,
 x.value('@handle[1]','varchar(128)') AS sql_handle,x.value('@begin[1]','int') AS statement_start,
 x.value('@end[1]','int') AS statement_end,x.value('@database[1]','int') AS database_id) i
LEFT JOIN sys.dm_exec_sessions s ON s.session_id=i.session_id AND s.login_time=CAST(CONVERT(datetime2,i.session_generation,126) AS datetime)
LEFT JOIN sys.dm_exec_requests r ON r.session_id=s.session_id AND r.request_id=i.request_id
 AND r.start_time=CAST(CONVERT(datetime2,i.request_generation,126) AS datetime) AND r.database_id=i.database_id
OUTER APPLY sys.dm_exec_sql_text(CASE WHEN r.sql_handle=CONVERT(varbinary(64),i.sql_handle,2)
 AND r.statement_start_offset=i.statement_start AND r.statement_end_offset=i.statement_end THEN r.sql_handle END) t
OUTER APPLY(SELECT CASE WHEN r.statement_start_offset>=0 AND r.statement_start_offset<=DATALENGTH(t.text)
 AND(r.statement_end_offset=-1 OR r.statement_end_offset>=r.statement_start_offset) THEN
 CASE WHEN r.statement_end_offset=-1 OR CONVERT(bigint,r.statement_end_offset)+2>DATALENGTH(t.text) THEN DATALENGTH(t.text)-r.statement_start_offset
 ELSE CONVERT(bigint,r.statement_end_offset)-r.statement_start_offset+2 END END AS source_bytes) fragment
"#;

const PREVIOUS_BATCH_BODY: &str = r#"
DECLARE /* culprit */ @ids xml=CONVERT(xml,@P1);
SELECT /* culprit */ i.session_id,i.session_generation,
 CASE WHEN s.session_id IS NOT NULL AND NOT EXISTS(SELECT 1 FROM sys.dm_exec_requests r WHERE r.session_id=s.session_id) THEN 1 ELSE 0 END AS matched,
 t.encrypted,DATALENGTH(t.text) AS source_bytes,CASE WHEN DATALENGTH(t.text)<=@P2 THEN t.text END AS payload
FROM @ids.nodes('/activity/row') n(x)
CROSS APPLY(SELECT x.value('@session[1]','int') AS session_id,x.value('@login[1]','varchar(40)') AS session_generation,
 x.value('@last_start[1]','varchar(40)') AS last_request_start,x.value('@last_end[1]','varchar(40)') AS last_request_end) i
LEFT JOIN sys.dm_exec_sessions s ON s.session_id=i.session_id AND s.login_time=CAST(CONVERT(datetime2,i.session_generation,126) AS datetime)
 AND s.last_request_start_time=CAST(CONVERT(datetime2,i.last_request_start,126) AS datetime) AND s.last_request_end_time=CAST(CONVERT(datetime2,i.last_request_end,126) AS datetime)
LEFT JOIN sys.dm_exec_connections c ON c.session_id=s.session_id AND c.parent_connection_id IS NULL
OUTER APPLY sys.dm_exec_sql_text(CASE WHEN NOT EXISTS(SELECT 1 FROM sys.dm_exec_requests r WHERE r.session_id=s.session_id) THEN c.most_recent_sql_handle END) t
"#;

pub(super) fn details(previous: bool, policy: QueryPolicy) -> String {
    let body = if previous { PREVIOUS_BATCH_BODY } else { CURRENT_SQL_BODY };
    format!("{body}{}", policy.hints())
}
