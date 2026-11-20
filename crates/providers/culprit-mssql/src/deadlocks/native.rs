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
    let original = super::system_health::ConfiguredTarget::read(io).await?;
    let events = original.events(io, from, until, limit, newest_first).await?;
    let current = super::system_health::ConfiguredTarget::read(io).await?;
    if !original.confirmed_by(&current) {
        return Err(culprit_core::telemetry::CollectionStop::SourceChanged.error());
    }
    Ok(events)
}

pub(super) async fn read_file<R: NativeRead>(
    io: &mut R,
    pattern: &str,
    from: &str,
    until: &str,
    limit: usize,
    newest_first: bool,
) -> Result<Vec<Event>> {
    let parameters = vec![
        Parameter::Integer(limit as i64),
        Parameter::Text(from.into()),
        Parameter::Text(until.into()),
        Parameter::Text(pattern.into()),
    ];
    let rows = io.query(&read_sql(newest_first), parameters, limit).await.map_err(ReadError::into_error)?;
    rows.iter().map(decode).collect()
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
fn decode(row: &Rec) -> Result<Event> {
    let xml = text(row, "event_xml")?;
    let at = canonical_time(&text(row, "at")?)?;
    let offset = integer(row, "file_offset")?;
    if offset < 0 {
        return Err(invalid_data());
    }
    let file = text(row, "file_name")?;
    let position =
        digest(&serde_json::to_vec(&("xml_deadlock_report", &file, offset, &at, &xml)).map_err(|_| invalid_data())?);
    let mut graphs = super::parse_with_time(&xml, "system_health", Some(&at))?;
    // Content/time/participants can repeat even within one native clock tick. Only this file decoder has
    // the original event position; imported XML retains its independent content identity.
    for (ordinal, graph) in graphs.iter_mut().enumerate() {
        graph.id = digest(
            &serde_json::to_vec(&("mssql-xel-deadlock-occurrence-v1", &position, ordinal))
                .map_err(|_| invalid_data())?,
        );
    }
    // Invalid/unparseable native XML must not look like an empty successful event.
    culprit_core::xml::read(&xml, |doc| doc.map(|_| ()).map_err(|_| invalid_data()))?;
    Ok(Event { position, file_id: digest(file.as_bytes()), offset: offset.to_string(), at, graphs })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::deadlocks::tests::{KEY_LOOKUP_XDL, event, file_row};
    use crate::{resources::test_support::FakeRead, value::Val};

    #[test]
    fn native_file_occurrence_identity_preserves_full_clocks_and_original_positions() {
        let at = "2026-10-11T12:05:01.1234567Z";
        let xml = event(at);
        let row = file_row(at, &xml);
        let first = decode(&row).unwrap();
        let replay = decode(&row).unwrap();
        let later_at = "2026-10-11T12:05:01.1234568Z";
        let later = decode(&file_row(later_at, &event(later_at))).unwrap();
        assert_eq!(first.at, first.graphs[0].at.as_deref().unwrap());
        assert_eq!(first.at, "2026-10-11T12:05:01.123456700Z");
        assert_eq!(first.position, replay.position, "an identical native event replays the same position");
        assert_eq!(first.graphs[0].id, replay.graphs[0].id);
        assert_eq!(first.offset, later.offset, "multiple native events can share one file-buffer offset");
        assert_ne!(first.position, later.position);
        assert_ne!(first.graphs[0].id, later.graphs[0].id);
        let moved = crate::resources::test_support::replace(
            &row,
            &[("file_name", Val::Text("/var/opt/mssql/log/system_health_1_134388425128000000.xel".into()))],
        );
        let moved = decode(&moved).unwrap();
        assert_ne!(first.position, moved.position, "native positions still include original file identity");
        assert_ne!(first.graphs[0].id, moved.graphs[0].id, "a copied origin cannot borrow the original occurrence ID");
        assert_eq!(first.graphs[0].processes, moved.graphs[0].processes);
        assert_eq!(first.graphs[0].resources, moved.graphs[0].resources);
        assert_eq!(decode(&row).unwrap().graphs.len(), 1, "shared file history is not attributed by database labels");
    }

    #[test]
    fn native_row_clock_precedes_identity_and_conflicting_or_invalid_event_clocks_are_refused() {
        let at = "2026-10-11T12:05:01.1234567Z";
        let bare = decode(&file_row(at, KEY_LOOKUP_XDL)).unwrap();
        let wrapped = decode(&file_row(at, &event(at))).unwrap();
        let mut same_content = wrapped.graphs[0].clone();
        same_content.id = bare.graphs[0].id.clone();
        assert_eq!(bare.graphs[0], same_content, "the graph content remains identical");
        assert_ne!(bare.position, wrapped.position, "original XML remains part of the native position");
        assert_ne!(bare.graphs[0].id, wrapped.graphs[0].id, "different native positions are separate occurrences");
        for event_at in ["2026-10-11T12:05:01.1234568Z", "2026-10-11T12:05:01.1234567", "bad-clock"] {
            assert!(decode(&file_row(at, &event(event_at))).is_err());
        }
        assert!(decode(&file_row("2026-10-11T12:05:01.1234567", &event(at))).is_err());
        for missing in [
            r#"<event name="xml_deadlock_report" timestamp="2026-10-11T12:05:01.1234567Z"><data name="xml_report"><value/></data></event>"#,
            r#"<event name="xml_deadlock_report" timestamp="bad-clock"><data name="xml_report"><value/></data></event>"#,
        ] {
            assert!(decode(&file_row(at, missing)).is_err(), "a selected report without a graph is not empty success");
        }
    }

    #[test]
    fn same_instant_graphs_with_different_original_positions_have_distinct_replay_stable_occurrence_ids() {
        use crate::resources::test_support::replace;
        let at = "2026-10-11T12:05:01.1234567Z";
        let xml = event(at);
        let original = file_row(at, &xml);
        let first = decode(&original).unwrap();
        let moved_offset = replace(&original, &[("file_offset", Val::Int(1024))]);
        let moved_file = replace(&original, &[("file_name", Val::Text("/var/opt/mssql/log/copied_health.xel".into()))]);
        for row in [&moved_offset, &moved_file] {
            let distinct = decode(row).unwrap();
            assert_eq!(first.at, distinct.at);
            assert_eq!(first.graphs[0].processes, distinct.graphs[0].processes);
            assert_ne!(first.position, distinct.position);
            assert_ne!(first.graphs[0].id, distinct.graphs[0].id);
            assert_eq!(distinct.graphs, decode(row).unwrap().graphs, "an exact native row replays its original ID");
        }
        assert_eq!(first.graphs, decode(&original).unwrap().graphs);
        let imported = super::super::parse_deadlock_xml(&xml, "pasted").unwrap();
        assert_eq!(imported, super::super::parse_deadlock_xml(&xml, "pasted").unwrap());
        assert_eq!(imported[0].id, super::super::parse_deadlock_xml(&xml, "system_health").unwrap()[0].id);
        assert_ne!(first.graphs[0].id, imported[0].id, "a source label cannot supply native position authority");
        assert_eq!(first.graphs[0].processes, imported[0].processes);
    }

    #[test]
    fn identical_graphs_inside_one_native_event_have_distinct_deterministic_ordinals() {
        let at = "2026-10-11T12:05:01.1234567Z";
        let graph = KEY_LOOKUP_XDL.replace("<deadlock-list>", "").replace("</deadlock-list>", "");
        let xml = format!(
            r#"<event name="xml_deadlock_report" timestamp="{at}"><data name="xml_report"><value>{graph}{graph}</value></data></event>"#
        );
        let row = file_row(at, &xml);
        let found = decode(&row).unwrap();
        let replay = decode(&row).unwrap();
        assert_eq!(found.graphs.len(), 2);
        assert_eq!(found.graphs, replay.graphs);
        assert_ne!(found.graphs[0].id, found.graphs[1].id);
        let mut second = found.graphs[1].clone();
        second.id = found.graphs[0].id.clone();
        assert_eq!(found.graphs[0], second, "only the native occurrence ordinal separates identical graphs");
        let imported = super::super::parse_deadlock_xml(&xml, "pasted").unwrap();
        assert_eq!(imported[0].id, imported[1].id, "imports keep their original content-ID contract");
    }

    #[tokio::test]
    async fn interactive_report_preserves_the_same_distinct_ids_as_the_shared_native_file_reader() {
        use crate::deadlocks::system_health::tests::configuration;
        use crate::resources::test_support::replace;
        use crate::value::Rec;
        let at = "2020-01-01T06:00:01.1234567Z";
        let first = file_row(at, &event(at));
        let second = replace(&first, &[("file_offset", Val::Int(1024))]);
        let rows = vec![first, second];
        let mut native = FakeRead::new(vec![Ok(rows.clone())]);
        let direct = read_file(
            &mut native,
            "/configured/system_health_*.xel",
            "2020-01-01T06:00:00Z",
            "2020-01-01T06:02:00Z",
            50,
            false,
        )
        .await
        .unwrap();
        let server = Rec::of(&[
            ("edition", Val::Int(3)),
            ("db", Val::Text("display-only-db".into())),
            ("observed_at", Val::Text("2020-01-01T06:02:00Z".into())),
        ]);
        let mut interactive =
            FakeRead::new(vec![Ok(vec![server]), Ok(vec![configuration()]), Ok(rows), Ok(vec![configuration()])]);
        let report = super::super::recent(&mut interactive, 1.0).await.unwrap();
        assert_eq!(report.deadlocks.len(), 2);
        let mut expected =
            direct.iter().flat_map(|event| &event.graphs).map(|graph| graph.id.clone()).collect::<Vec<_>>();
        let mut actual = report.deadlocks.iter().map(|graph| graph.id.clone()).collect::<Vec<_>>();
        expected.sort();
        actual.sort();
        assert_eq!(actual, expected, "scan and collector native reads share one occurrence decoder");
        assert_ne!(actual[0], actual[1]);
        assert_eq!(report.scope, Some(culprit_core::MeasurementScope::Server));
        assert!(report.recent_only && report.note.is_some());
        assert_eq!((native.calls.len(), interactive.calls.len()), (1, 4));
        assert_eq!((native.closed, interactive.closed), (0, 0), "the same caller retains close ownership");
    }

    #[tokio::test]
    async fn system_health_uses_native_configured_paths_and_rechecks_the_same_owner_without_flushing_views() {
        use crate::deadlocks::system_health::tests::configuration;
        use crate::resources::test_support::replace;
        for (path, pattern) in [
            ("/var/opt/mssql/log/system_health.xel", "/var/opt/mssql/log/system_health_*.xel"),
            (r"C:\SQL logs\health's events.xel", r"C:\SQL logs\health's events_*.xel"),
        ] {
            for newest_first in [false, true] {
                let configured = replace(&configuration(), &[("file_path", Val::Text(path.into()))]);
                let mut io = FakeRead::new(vec![Ok(vec![configured.clone()]), Ok(vec![]), Ok(vec![configured])]);
                assert!(
                    read(
                        &mut io,
                        Origin::SystemHealth,
                        "unused-label",
                        "2020-01-01T06:00:00Z",
                        "2020-01-01T07:00:00Z",
                        10,
                        newest_first
                    )
                    .await
                    .unwrap()
                    .is_empty()
                );
                assert_eq!(io.calls.len(), 3);
                assert!(matches!(&io.calls[1].parameters[3], Parameter::Text(bound) if bound==pattern));
                assert!(!io.calls[1].sql.contains(path));
                let order = if newest_first { " DESC" } else { " ASC" };
                assert!(io.calls[1].sql.contains(order));
                assert!(
                    io.calls
                        .iter()
                        .all(|call| !call.sql.contains("dm_xe_session_targets") && !call.sql.contains("target_data"))
                );
            }
        }
    }

    #[tokio::test]
    async fn unavailable_or_changed_native_file_configuration_never_becomes_successful_empty_history() {
        use crate::deadlocks::system_health::tests::configuration;
        use crate::resources::test_support::replace;
        let mut io = FakeRead::new(vec![Ok(vec![replace(
            &configuration(),
            &[("file_path", Val::Text("system_health.xel".into()))],
        )])]);
        assert_eq!(
            read(&mut io, Origin::SystemHealth, "db", "2020-01-01T06:00:00Z", "2020-01-01T07:00:00Z", 10, false)
                .await
                .err()
                .unwrap()
                .code,
            culprit_core::ErrorCode::Unsupported
        );
        assert_eq!(io.calls.len(), 1);
        let mut dropped = FakeRead::new(vec![Ok(vec![configuration()]), Ok(vec![]), Ok(vec![])]);
        let missing =
            read(&mut dropped, Origin::SystemHealth, "db", "2020-01-01T06:00:00Z", "2020-01-01T07:00:00Z", 10, false)
                .await
                .err()
                .unwrap();
        assert_eq!(missing.code, culprit_core::ErrorCode::Unsupported);
        assert_eq!(dropped.calls.len(), 3);
        for change in [
            ("event_session_id", Val::Int(2)),
            ("target_id", Val::Int(2)),
            ("observed_at", Val::Text("2020-01-01T06:01:59Z".into())),
        ] {
            let mut io = FakeRead::new(vec![
                Ok(vec![configuration()]),
                Ok(vec![]),
                Ok(vec![replace(&configuration(), &[(change.0, change.1)])]),
            ]);
            let error =
                read(&mut io, Origin::SystemHealth, "db", "2020-01-01T06:00:00Z", "2020-01-01T07:00:00Z", 10, false)
                    .await
                    .err()
                    .unwrap();
            assert_eq!(
                culprit_core::telemetry::CollectionStop::from_error(&error),
                culprit_core::telemetry::CollectionStop::SourceChanged
            );
            assert_eq!(io.calls.len(), 3);
        }
    }
}
