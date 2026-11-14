use super::*;
use crate::{
    connect::Flavor,
    native::metadata::{ConnectionContext, Metadata},
};
use culprit_core::DatabaseIdentity;

pub(crate) fn metadata() -> Metadata {
    Metadata {
        connection: ConnectionContext {
            database: DatabaseIdentity::server(culprit_core::Engine::Mysql, "shop", "fixture.invalid", 3306, None),
            database_name: "shop".into(),
            server_id: 1,
            server_name: "fixture".into(),
            connection_id: 90,
            version: "8.4.0".into(),
            flavor: Flavor::Mysql,
            login: "monitor@localhost".into(),
            at: "2020-01-01T06:00:00Z".into(),
            performance_schema: true,
            read_only: false,
        },
        database_wide: true,
        process_wide: true,
        started_second: Some(1),
    }
}
pub(crate) fn consumers(enabled: bool) -> Vec<Rec> {
    ["global_instrumentation", "thread_instrumentation", "events_statements_current"]
        .map(|name| {
            Rec::from_values(vec![
                ("name", Value::from(name)),
                ("enabled", Value::from(if enabled { "YES" } else { "NO" })),
            ])
        })
        .into()
}
pub(crate) fn row(id: u64, command: &str, current: bool) -> Rec {
    row_with(id, command, current, &[])
}
pub(crate) fn row_with(id: u64, command: &str, current: bool, changes: &[(&str, Value)]) -> Rec {
    let mut values = vec![
        ("id", Value::from(id)),
        ("thread_id", Value::from(id + 100)),
        ("db", Value::from("shop")),
        ("user_name", Value::from("app")),
        ("host", Value::from("client")),
        ("command", Value::from(command)),
        ("state", Value::from("executing")),
        ("seconds", Value::from(0u64)),
        ("instrumented", Value::from("YES")),
        ("event_id", Value::from(3u64)),
        ("timer_start", Value::from(4u64)),
        ("elapsed_ps", Value::from(10_000_000_000u64)),
        ("end_event_id", if current { Value::NULL } else { Value::from(3u64) }),
        ("request_db", Value::from("shop")),
        ("query_hash", Value::from("native-digest")),
        ("observed_at", Value::from("2020-01-01T06:00:00.100000Z")),
    ];
    for (name, value) in changes {
        values.iter_mut().find(|(column, _)| column == name).expect("known native activity field").1 = value.clone();
    }
    Rec::from_values(values)
}
