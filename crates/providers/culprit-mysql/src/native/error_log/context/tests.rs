use super::*;
use crate::{read::test_support::Read, rows::Rec};
use culprit_core::{Engine, telemetry::*};
use mysql_async::Value;

pub(crate) const UUID: &str = "f54a461c-0001-4000-8000-000000000001";

pub(crate) fn platform(version: &str, instrumentation: bool) -> Rec {
    platform_at(version, instrumentation, "2026-11-13T10:00:00.123456Z")
}

pub(crate) fn platform_at(version: &str, instrumentation: bool, at: &str) -> Rec {
    Rec::from_values(vec![
        ("version", Value::from(version)),
        ("performance_schema", Value::UInt(u64::from(instrumentation))),
        ("connection_id", Value::UInt(55)),
        ("login", Value::from("monitor@%")),
        ("server_id", Value::UInt(9)),
        ("server_name", Value::from("native-host")),
        ("observed_at", Value::from(at)),
    ])
}

pub(crate) fn configuration(uuid: &str, services: &str) -> Rec {
    Rec::from_values(vec![("server_uuid", Value::from(uuid)), ("log_error_services", Value::from(services))])
}

pub(crate) fn process(observed: u64, uptime: u64) -> Rec {
    Rec::from_values(vec![("observed_second", Value::UInt(observed)), ("uptime", Value::UInt(uptime))])
}

pub(crate) fn rows() -> Vec<Result<Vec<Rec>>> {
    vec![
        Ok(vec![platform("8.4.0", true)]),
        Ok(vec![configuration(UUID, "log_filter_internal; log_sink_internal")]),
        Ok(vec![process(10_000, 100)]),
    ]
}

pub(crate) fn resource() -> SharedResourceIdentity {
    SharedResourceIdentity {
        engine: Engine::Mysql,
        kind: SharedResourceKind::Instance,
        namespace: NativeIdentity::new("mysql-server").qualified("server_uuid", UUID),
        native: NativeIdentity::new(UUID),
    }
}

#[tokio::test]
async fn native_context_is_instance_owned_and_never_uses_the_selected_database_or_route_alias() {
    let mut reader = Read::with(rows());
    let (context, at) = read(&mut reader).await.unwrap();
    assert_eq!(context.instance.resource, resource());
    assert_eq!(context.instance.started_second, 9_900);
    assert_eq!(at, "2026-11-13T10:00:00.123456Z");
    assert_eq!(reader.queries.len(), 3);
    assert!(reader.queries.iter().all(|(sql, _, _)| !sql.contains("SCHEMATA")));
    assert_eq!(reader.closed, 0);
}

#[tokio::test]
async fn incompatible_platforms_stop_before_mysql_specific_configuration_or_error_log_sql() {
    for (version, instrumentation) in [("11.8.0-MariaDB", true), ("8.0.21", true), ("8.4.0", false)] {
        let mut reader = Read::with(vec![Ok(vec![platform(version, instrumentation)])]);
        assert_eq!(read(&mut reader).await.err().unwrap().code, ErrorCode::Unsupported);
        assert_eq!(reader.queries.len(), 1, "{version}");
        assert!(!reader.queries[0].0.contains("server_uuid"));
    }
}

#[tokio::test]
async fn source_context_preserves_empty_current_pipeline_without_erasing_older_retained_rows() {
    let mut replies = rows();
    replies[1] = Ok(vec![configuration(UUID, "")]);
    let mut reader = Read::with(replies);
    let (context, _) = read(&mut reader).await.unwrap();
    assert!(context.log_error_services.is_empty());
}

#[tokio::test]
async fn malformed_native_identity_and_unknown_process_start_do_not_acquire_shared_ownership() {
    for uuid in ["", "not-a-native-uuid", "f54a461c-0001-4000-8000-00000000000g"] {
        let mut replies = rows();
        replies[1] = Ok(vec![configuration(uuid, "log_sink_internal")]);
        let mut reader = Read::with(replies);
        assert!(read(&mut reader).await.is_err());
        assert_eq!(reader.queries.len(), 2);
    }
    let mut replies = rows();
    replies[2] = Ok(vec![process(10, 100)]);
    let mut reader = Read::with(replies);
    assert!(read(&mut reader).await.is_err());
    assert_eq!(reader.queries.len(), 3);
}
