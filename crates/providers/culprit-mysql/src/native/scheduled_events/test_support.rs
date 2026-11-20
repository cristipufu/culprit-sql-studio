use super::*;
use crate::read::test_support::Read;
use culprit_core::{ConnectionSpec, Engine};
pub(crate) fn connector() -> std::sync::Arc<Connector> {
    std::sync::Arc::new(
        Connector::new(&ConnectionSpec {
            engine: Some(Engine::Mysql),
            host: "127.0.0.1".into(),
            port: Some(1),
            database: Some("shop".into()),
            user: Some("unused".into()),
            password: Some("unused".into()),
            ..Default::default()
        })
        .unwrap(),
    )
}
pub(crate) fn context(at: &str) -> Vec<Result<Vec<Rec>>> {
    vec![
        Ok(vec![Rec::from_values(vec![
            ("database_name", Value::from("shop")),
            ("server_id", Value::UInt(1)),
            ("server_name", Value::from("native-server")),
            ("connection_id", Value::UInt(55)),
            ("version", Value::from("8.4.11")),
            ("login", Value::from("observer@localhost")),
            ("performance_schema", Value::UInt(1)),
            ("read_only", Value::UInt(0)),
            ("observed_at", Value::from(at)),
        ])]),
        Ok(vec![Rec::from_values(vec![
            ("server_uuid", Value::from("aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee")),
            ("event_scheduler", Value::from("OFF")),
            ("session_time_zone", Value::from("+00:00")),
            ("event_visibility", Value::UInt(1)),
            ("observed_at", Value::from(at)),
        ])]),
        Ok(vec![Rec::from_values(vec![("observed_second", Value::UInt(1000)), ("uptime", Value::UInt(100))])]),
    ]
}
pub(crate) fn event(name: &str) -> Rec {
    Rec::from_values(vec![
        ("source_connection_id", Value::UInt(55)),
        ("source_member", Value::from("aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee")),
        ("source_read_only", Value::UInt(0)),
        ("source_time_zone", Value::from("+00:00")),
        ("source_scheduler", Value::from("OFF")),
        ("event_schema", Value::from("shop")),
        ("event_name", Value::from(name)),
        ("status", Value::from("DISABLED")),
        ("event_type", Value::from("RECURRING")),
        ("time_zone", Value::from("SYSTEM")),
        ("interval_value", Value::from("1")),
        ("interval_field", Value::from("DAY")),
        ("execute_at", Value::NULL),
        ("starts", Value::from("2026-11-20T13:00:00.000000")),
        ("ends", Value::NULL),
        ("last_started", Value::NULL),
        ("created", Value::from("2026-11-20T10:00:00.000000")),
        ("last_altered", Value::from("2026-11-20T10:00:00.000000")),
        ("observed_at", Value::from("2026-11-20T12:00:01.000000Z")),
    ])
}
pub(crate) fn replies(events: Vec<Rec>) -> Vec<Result<Vec<Rec>>> {
    let mut rows = context("2026-11-20T12:00:00Z");
    rows.push(Ok(sql::COLUMNS
        .iter()
        .map(|(name, date)| {
            Rec::from_values(vec![
                ("column_name", Value::from(*name)),
                ("data_type", Value::from(if *date { "datetime" } else { "varchar" })),
            ])
        })
        .collect()));
    let events = if events.is_empty() { vec![empty()] } else { events };
    rows.push(Ok(events));
    rows.extend(context("2026-11-20T12:00:02Z"));
    rows
}
pub(crate) fn reader(events: Vec<Rec>) -> Read {
    Read::with(replies(events))
}

pub(crate) fn empty() -> Rec {
    event("unused").with_fields(
        &[
            "event_schema",
            "event_name",
            "status",
            "event_type",
            "time_zone",
            "interval_value",
            "interval_field",
            "execute_at",
            "starts",
            "ends",
            "last_started",
            "created",
            "last_altered",
        ]
        .into_iter()
        .map(|name| (name, Value::NULL))
        .collect::<Vec<_>>(),
    )
}
