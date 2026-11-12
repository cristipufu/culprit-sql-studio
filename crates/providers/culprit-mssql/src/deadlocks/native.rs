//! Native retained event reads. A successful bounded read does not establish the retention horizon.
use crate::{
    read::{NativeRead, Parameter, ReadError, integer, invalid_data, text},
    value::Rec,
};
use culprit_core::{
    Deadlock, Result,
    telemetry::{canonical_time, digest},
};

pub(crate) const SERVER: &str = "SELECT /* culprit */ CAST(SERVERPROPERTY('EngineEdition') AS int) AS edition,DB_NAME() AS db,CONVERT(varchar(40),SYSUTCDATETIME(),127)+'Z' AS observed_at";
pub(crate) const RING: &str = "SELECT /* culprit */ CAST(t.target_data AS nvarchar(max)) AS target_xml FROM sys.dm_xe_session_targets t JOIN sys.dm_xe_sessions s ON s.address=t.event_session_address WHERE s.name=N'system_health' AND t.target_name=N'ring_buffer'";
const FILE_TARGET: &str = "SELECT TOP(1) /* culprit */ CAST(t.target_data AS xml).value('(EventFileTarget/File/@name)[1]','nvarchar(4000)') AS file_path FROM sys.dm_xe_session_targets t JOIN sys.dm_xe_sessions s ON s.address=t.event_session_address WHERE s.name=N'system_health' AND t.target_name=N'event_file'";

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Origin {
    DatabaseSession,
    SystemHealth,
}
/// Kept private and never serialized. The source graph may contain identities and SQL literals.
pub(crate) struct Event {
    pub position: String,
    pub file_id: String,
    pub offset: String,
    pub at: String,
    pub graphs: Vec<Deadlock>,
}

pub(crate) async fn read<R: NativeRead>(
    io: &mut R,
    origin: Origin,
    database: &str,
    from: &str,
    until: &str,
    limit: usize,
    newest_first: bool,
) -> Result<Vec<Event>> {
    if origin == Origin::DatabaseSession {
        return super::database::read(io, database, from, until, limit, newest_first).await;
    }
    let sql = read_sql(newest_first);
    let mut parameters =
        vec![Parameter::Integer(limit as i64), Parameter::Text(from.into()), Parameter::Text(until.into())];
    // Resolve the active target rather than relying on the process's default directory. A relative
    // system_health wildcard can return an empty set even while the configured target contains events.
    let target = io.query(FILE_TARGET, vec![], 1).await.map_err(ReadError::into_error)?;
    let path = text(target.first().ok_or_else(invalid_data)?, "file_path")?;
    parameters.push(Parameter::Text(file_pattern(&path)?));
    let rows = io.query(&sql, parameters, limit).await.map_err(ReadError::into_error)?;
    rows.iter().map(|row| decode(row, database)).collect()
}
fn file_pattern(path: &str) -> Result<String> {
    let (stem, extension) = path.rsplit_once('.').ok_or_else(invalid_data)?;
    let (numbered, timestamp) = stem.rsplit_once('_').ok_or_else(invalid_data)?;
    let (prefix, number) = numbered.rsplit_once('_').ok_or_else(invalid_data)?;
    if !extension.eq_ignore_ascii_case("xel")
        || prefix.is_empty()
        || prefix.contains(['*', '?'])
        || number.is_empty()
        || timestamp.is_empty()
        || !number.bytes().chain(timestamp.bytes()).all(|byte| byte.is_ascii_digit())
    {
        return Err(invalid_data());
    }
    Ok(format!("{prefix}_*.{extension}"))
}
pub(super) fn read_sql(newest_first: bool) -> String {
    let direction = if newest_first { "DESC" } else { "ASC" };
    // SQL Server 2022 can discard real events when a timestamp predicate is pushed into the XEL reader.
    // Convert the returned timestamp explicitly, as Microsoft's documented system_health example does:
    // https://learn.microsoft.com/sql/relational-databases/system-functions/sys-fn-xe-file-target-read-file-transact-sql
    let timestamp = "CAST(x.timestamp_utc AS datetime2(7))";
    format!(
        "SELECT TOP (@P1) /* culprit */ x.file_name,x.file_offset,CONVERT(varchar(40),{timestamp},127)+'Z' AS at,CAST(x.event_data AS nvarchar(max)) AS event_xml FROM sys.fn_xe_file_target_read_file(@P4,NULL,NULL,NULL) x WHERE x.object_name=N'xml_deadlock_report' AND {timestamp}>=CONVERT(datetime2(7),@P2,127) AND {timestamp}<CONVERT(datetime2(7),@P3,127) ORDER BY {timestamp} {direction},x.file_name {direction},x.file_offset {direction},HASHBYTES('SHA2_256',CAST(x.event_data AS nvarchar(max))) {direction} OPTION(MAXDOP 1)"
    )
}
fn decode(row: &Rec, database: &str) -> Result<Event> {
    let xml = text(row, "event_xml")?;
    let at = canonical_time(&text(row, "at")?)?;
    let offset = integer(row, "file_offset")?;
    if offset < 0 {
        return Err(invalid_data());
    }
    let file = text(row, "file_name")?;
    let position =
        digest(&serde_json::to_vec(&("xml_deadlock_report", &file, offset, &at, &xml)).map_err(|_| invalid_data())?);
    let graphs = super::parse_with_time(&xml, "system_health", Some(at.clone()))?
        .into_iter()
        .filter(|graph| super::involves(graph, database))
        .collect();
    // Invalid/unparseable native XML must not look like an empty successful event.
    culprit_core::xml::read(&xml, |doc| doc.map(|_| ()).map_err(|_| invalid_data()))?;
    Ok(Event { position, file_id: digest(file.as_bytes()), offset: offset.to_string(), at, graphs })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{resources::test_support::FakeRead, value::Val};
    #[tokio::test]
    async fn system_health_uses_its_observed_target_and_binds_the_rollover_pattern_as_data() {
        for path in [
            "/var/opt/mssql/log/system_health_0_134388425128000000.xel",
            r"C:\SQL logs\health's events_3_134388425128000000.xel",
        ] {
            let mut io = FakeRead::new(vec![Ok(vec![Rec::of(&[("file_path", Val::Text(path.into()))])]), Ok(vec![])]);
            assert!(
                read(&mut io, Origin::SystemHealth, "db", "2020-01-01T06:00:00Z", "2020-01-01T07:00:00Z", 10, false)
                    .await
                    .unwrap()
                    .is_empty()
            );
            assert_eq!(io.calls.len(), 2);
            assert!(io.calls[0].sql.contains("event_file"));
            assert!(io.calls[1].sql.contains("fn_xe_file_target_read_file(@P4"));
            assert!(!io.calls[1].sql.contains(path));
            assert!(
                matches!(&io.calls[1].parameters[3],Parameter::Text(pattern) if pattern==&file_pattern(path).unwrap())
            );
        }
        for path in ["", "health.xel", "health_bad_123.xel", "health_0_bad.xel", "health*_0_123.xel"] {
            assert!(file_pattern(path).is_err(), "unrecognized native target: {path}");
        }
    }
}
