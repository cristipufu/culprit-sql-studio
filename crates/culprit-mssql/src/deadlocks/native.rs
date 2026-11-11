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

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Origin {
    AzureTelemetry,
    SystemHealth,
}
impl Origin {
    fn relation(self) -> &'static str {
        match self {
            Self::AzureTelemetry => "sys.fn_xe_telemetry_blob_target_read_file('dl',NULL,NULL,NULL)",
            Self::SystemHealth => "sys.fn_xe_file_target_read_file(N'system_health*.xel',NULL,NULL,NULL)",
        }
    }
    fn event(self) -> &'static str {
        match self {
            Self::AzureTelemetry => "database_xml_deadlock_report",
            Self::SystemHealth => "xml_deadlock_report",
        }
    }
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
    let sql = read_sql(origin, newest_first);
    let rows = io
        .query(
            &sql,
            vec![Parameter::Integer(limit as i64), Parameter::Text(from.into()), Parameter::Text(until.into())],
            limit,
        )
        .await
        .map_err(ReadError::into_error)?;
    rows.iter().map(|row| decode(row, origin, database)).collect()
}
pub(super) fn read_sql(origin: Origin, newest_first: bool) -> String {
    let direction = if newest_first { "DESC" } else { "ASC" };
    format!(
        "SELECT TOP (@P1) /* culprit */ x.file_name,x.file_offset,CONVERT(varchar(40),x.timestamp_utc,127)+'Z' AS at,CAST(x.event_data AS nvarchar(max)) AS event_xml FROM {} x WHERE x.object_name=N'{}' AND x.timestamp_utc>=CONVERT(datetime2(7),@P2,127) AND x.timestamp_utc<CONVERT(datetime2(7),@P3,127) ORDER BY x.timestamp_utc {direction},x.file_name {direction},x.file_offset {direction},HASHBYTES('SHA2_256',CAST(x.event_data AS nvarchar(max))) {direction} OPTION(MAXDOP 1)",
        origin.relation(),
        origin.event()
    )
}
fn decode(row: &Rec, origin: Origin, database: &str) -> Result<Event> {
    let xml = text(row, "event_xml")?;
    let at = canonical_time(&text(row, "at")?)?;
    let offset = integer(row, "file_offset")?;
    if offset < 0 {
        return Err(invalid_data());
    }
    let file = text(row, "file_name")?;
    let position =
        digest(&serde_json::to_vec(&(origin.event(), &file, offset, &at, &xml)).map_err(|_| invalid_data())?);
    let graphs = match origin {
        Origin::AzureTelemetry => super::telemetry_deadlocks(&xml, database, Some(at.clone()))?,
        Origin::SystemHealth => super::parse_with_time(&xml, "system_health", Some(at.clone()))?
            .into_iter()
            .filter(|graph| super::involves(graph, database))
            .collect(),
    };
    // Invalid/unparseable native XML must not look like an empty successful event.
    culprit_core::xml::read(&xml, |doc| doc.map(|_| ()).map_err(|_| invalid_data()))?;
    Ok(Event { position, file_id: digest(file.as_bytes()), offset: offset.to_string(), at, graphs })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{resources::test_support::FakeRead, value::Val};
    #[tokio::test]
    async fn native_event_identity_keeps_fractional_time_and_file_position() {
        let xml = "<event><data name=\"database_name\"><value>other</value></data></event>";
        let row = Rec::of(&[
            ("event_xml", Val::Text(xml.into())),
            ("file_name", Val::Text("native-file".into())),
            ("file_offset", Val::Int(9007199254740993)),
            ("at", Val::Text("2020-01-01T06:00:00.1533333Z".into())),
        ]);
        let mut io = FakeRead::new(vec![Ok(vec![row])]);
        let events =
            read(&mut io, Origin::AzureTelemetry, "db", "2020-01-01T06:00:00Z", "2020-01-01T07:00:00Z", 10, false)
                .await
                .unwrap();
        let event = &events[0];
        assert_eq!(event.at, "2020-01-01T06:00:00.153333300Z");
        assert_eq!(event.offset, "9007199254740993");
        assert_eq!(event.position.len(), 64);
        assert!(event.graphs.is_empty());
        assert_eq!(io.calls.len(), 1);
    }
}
