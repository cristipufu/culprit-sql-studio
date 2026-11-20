//! Original native session identification and independent samples shared by qualification and owned controls.
use super::*;
use crate::{activity::IdentityDetail, local_source};
use culprit_core::telemetry::*;

#[derive(Clone)]
pub(super) struct SessionIdentity {
    pub(super) id: i64,
    pub(super) login: String,
}
#[derive(Clone)]
pub(super) struct Counters {
    pub(super) cpu_ms: u64,
    pub(super) logical_reads: u64,
    pub(super) reads: u64,
    pub(super) dirty_marks: u64,
}
pub(super) struct Sample {
    pub(super) observed_at: String,
    pub(super) counters: Counters,
    pub(super) memory_pages: Option<u64>,
    pub(super) active: bool,
    #[cfg(test)]
    pub(super) wait: Option<String>,
    #[cfg(test)]
    pub(super) blocker: Option<i64>,
    #[cfg(test)]
    pub(super) granted_kb: Option<u64>,
    #[cfg(test)]
    pub(super) requested_kb: Option<u64>,
    #[cfg(test)]
    pub(super) used_kb: Option<u64>,
}
pub(super) struct PreparedSession {
    pub(super) identity: SessionIdentity,
    pub(super) context: NativeReadContext,
    pub(super) authority: local_source::Context,
    #[cfg(test)]
    pub(super) metadata: Rec,
}
pub(super) async fn prepare<R: NativeRead>(
    reader: &mut R,
    host: &str,
    port: u16,
    detail: IdentityDetail,
) -> Result<PreparedSession> {
    if host.trim().is_empty() || host.contains('\0') || port == 0 {
        return Err(invalid_data());
    }
    let rows = reader.query(IDENTITY_SQL, vec![], 1).await.map_err(ReadError::into_error)?;
    let [row] = rows.as_slice() else {
        return Err(invalid_data());
    };
    let identity = SessionIdentity {
        id: integer(row, "session_id")?,
        login: crate::sql_time::native_time(&text(row, "login_time")?)?,
    };
    if !(1..=i16::MAX as i64).contains(&identity.id) || identity.login.starts_with("1900-01-01") {
        return Err(invalid_data());
    }
    let (authority, _metadata) =
        local_source::read_initial(reader, host, port, detail, local_source::Detail::Authority).await?;
    let context = authority.completed(&authority)?;
    if !authority.is_qualified() || !context.has_qualified_local_owner() {
        return Err(invalid_data());
    }
    Ok(PreparedSession {
        identity,
        context,
        authority,
        #[cfg(test)]
        metadata: _metadata,
    })
}
pub(super) async fn observer_scope<R: NativeRead>(
    reader: &mut R,
    source: &NativeReadContext,
    host: &str,
    port: u16,
) -> Result<NativeReadContext> {
    let authority = local_source::Context::read(reader, host, port).await?;
    let context = authority.completed(&authority)?;
    if !context.matches_earlier_scope(source) {
        return Err(CollectionStop::SourceChanged.error());
    }
    Ok(context)
}
pub(super) async fn sample<R: NativeRead>(reader: &mut R, identity: &SessionIdentity) -> Result<Option<Sample>> {
    let rows = reader
        .query(SAMPLE_SQL, vec![Parameter::Integer(identity.id), Parameter::Text(identity.login.clone())], 2)
        .await
        .map_err(ReadError::into_error)?;
    let [row] = rows.as_slice() else {
        return if rows.is_empty() { Ok(None) } else { Err(CollectionStop::SourceChanged.error()) };
    };
    Ok(Some(Sample {
        observed_at: canonical_time(&text(row, "observed_at")?)?,
        counters: Counters::read(row)?,
        memory_pages: optional_count(row, "memory_pages")?,
        active: boolean(row, "active")?,
        #[cfg(test)]
        wait: optional_text(row, "wait_type")?,
        #[cfg(test)]
        blocker: optional_signed_integer(row, "blocking_session_id")?,
        #[cfg(test)]
        granted_kb: optional_count(row, "granted_kb")?,
        #[cfg(test)]
        requested_kb: optional_count(row, "requested_kb")?,
        #[cfg(test)]
        used_kb: optional_count(row, "used_kb")?,
    }))
}
impl Counters {
    pub(super) fn read(row: &Rec) -> Result<Self> {
        Ok(Self {
            cpu_ms: count(row, "cpu_ms")?,
            logical_reads: count(row, "logical_reads")?,
            reads: count(row, "reads")?,
            dirty_marks: count(row, "dirty_marks")?,
        })
    }
}
fn count(row: &Rec, key: &str) -> Result<u64> {
    optional_counter(row, key)?.ok_or_else(invalid_data)?.parse().map_err(|_| invalid_data())
}
fn optional_count(row: &Rec, key: &str) -> Result<Option<u64>> {
    optional_counter(row, key)?.map(|value| value.parse().map_err(|_| invalid_data())).transpose()
}
const IDENTITY_SQL: &str = "SELECT /* culprit */ CONVERT(bigint,@@SPID) AS session_id,CONVERT(varchar(33),CONVERT(datetime2(7),login_time),126) AS login_time FROM sys.dm_exec_sessions WHERE session_id=@@SPID";
const SAMPLE_SQL: &str = "SELECT /* culprit */ CONVERT(varchar(40),SYSUTCDATETIME(),127)+'Z' AS observed_at,CONVERT(bigint,s.cpu_time) AS cpu_ms,s.logical_reads,s.reads,s.writes AS dirty_marks,s.memory_usage AS memory_pages,CAST(CASE WHEN r.session_id IS NULL THEN 0 ELSE 1 END AS bit) AS active,r.wait_type,r.blocking_session_id,CONVERT(bigint,g.granted_memory_kb) AS granted_kb,CONVERT(bigint,g.requested_memory_kb) AS requested_kb,CONVERT(bigint,g.used_memory_kb) AS used_kb FROM sys.dm_exec_sessions s LEFT JOIN sys.dm_exec_requests r ON r.session_id=s.session_id LEFT JOIN sys.dm_exec_query_memory_grants g ON g.session_id=r.session_id AND g.request_id=r.request_id WHERE s.session_id=@P1 AND CONVERT(datetime2(7),s.login_time)=CONVERT(datetime2(7),@P2,126) OPTION(MAXDOP 1)";
