use super::*;
use crate::{
    resources::test_support::{FakeRead, replace},
    value::{Rec, Val},
};

pub(crate) fn configuration() -> Rec {
    Rec::of(&[
        ("observed_at", Val::Text("2020-01-01T06:02:00Z".into())),
        ("event_session_id", Val::Int(1)),
        ("target_id", Val::Int(1)),
        ("target_package", Val::Text("package0".into())),
        ("file_path_type", Val::Text("nvarchar".into())),
        ("file_path_bytes", Val::Int(82)),
        ("file_path", Val::Text("/var/opt/mssql/log/system_health.xel".into())),
    ])
}

#[test]
fn only_native_absolute_configured_prefixes_become_parameter_data_without_directory_discovery() {
    for (path, pattern) in [
        ("/var/opt/mssql/log/system_health.xel", "/var/opt/mssql/log/system_health_*.xel"),
        (r"C:\SQL logs\health's events.xel", r"C:\SQL logs\health's events_*.xel"),
        (r"\\server\share\health.xel", r"\\server\share\health_*.xel"),
    ] {
        assert_eq!(configured_pattern(path).unwrap(), pattern);
    }
    for path in [
        "system_health.xel",
        "../system_health.xel",
        r"C:system_health.xel",
        "/logs/*.xel",
        "/logs/health?.xel",
        "https://account.blob.core.windows.net/container/health.xel",
        "/logs/health.xel?sig=private-canary",
        "/logs/health.txt",
        "/logs/.xel",
    ] {
        assert_eq!(configured_pattern(path).unwrap_err().code, ErrorCode::Unsupported, "{path}");
    }
}

#[tokio::test]
async fn configured_metadata_does_not_read_target_data_or_flushing_target_views() {
    let mut io = FakeRead::new(vec![Ok(vec![configuration()])]);
    let target = ConfiguredTarget::read(&mut io).await.unwrap();
    assert_eq!(target.observed_at(), "2020-01-01T06:02:00.000000000Z");
    assert_eq!(io.calls.len(), 1);
    let sql = &io.calls[0].sql;
    assert!(sql.contains("sys.server_event_session_fields"));
    assert!(sql.contains("TOP(2)"));
    for absent in [
        "target_data",
        "dm_xe_sessions",
        "dm_xe_session_targets",
        "create_time",
        "fn_xe_file_target_read_file",
        "ALTER",
        "CREATE",
    ] {
        assert!(!sql.contains(absent), "{absent}");
    }
    assert_eq!(io.close().await, culprit_core::telemetry::SourceShutdown::Quiescent);
}

#[tokio::test]
async fn missing_ambiguous_hidden_or_relative_configuration_never_falls_back_to_a_runtime_target() {
    for rows in [
        vec![],
        vec![configuration(), configuration()],
        vec![replace(&configuration(), &[("file_path", Val::Null)])],
        vec![replace(&configuration(), &[("file_path_bytes", Val::Null)])],
        vec![replace(&configuration(), &[("file_path", Val::Text("system_health.xel".into()))])],
    ] {
        let mut io = FakeRead::new(vec![Ok(rows)]);
        assert_eq!(ConfiguredTarget::read(&mut io).await.err().unwrap().code, ErrorCode::Unsupported);
        assert_eq!(io.calls.len(), 1, "no target-data or directory fallback");
        assert_eq!(io.close().await, culprit_core::telemetry::SourceShutdown::Quiescent);
    }
}

#[tokio::test]
async fn persistent_catalog_configuration_owns_file_selection_without_active_emitter_continuity() {
    let mut io = FakeRead::new(vec![Ok(vec![configuration()])]);
    let original = ConfiguredTarget::read(&mut io).await.unwrap();
    assert!(original.confirmed_by(&original), "equal native UTC ticks already qualify an unchanged witness");
    for (field, value) in [
        ("event_session_id", Val::Int(2)),
        ("target_id", Val::Int(2)),
        ("file_path", Val::Text("/var/opt/mssql/log/other-health.xel".into())),
    ] {
        let mut io = FakeRead::new(vec![Ok(vec![replace(&configuration(), &[(field, value)])])]);
        let changed = ConfiguredTarget::read(&mut io).await.unwrap();
        assert!(!original.same_configuration(&changed), "{field}");
    }
    let mut io = FakeRead::new(vec![Ok(vec![replace(
        &configuration(),
        &[("observed_at", Val::Text("2020-01-01T06:03:00Z".into()))],
    )])]);
    let later = ConfiguredTarget::read(&mut io).await.unwrap();
    assert!(original.same_configuration(&later) && original.confirmed_by(&later));
    let mut io = FakeRead::new(vec![Ok(vec![replace(
        &configuration(),
        &[("observed_at", Val::Text("2020-01-01T06:01:59Z".into()))],
    )])]);
    let backward = ConfiguredTarget::read(&mut io).await.unwrap();
    assert!(original.same_configuration(&backward));
    assert!(!original.confirmed_by(&backward), "a genuine backward clock remains refused");
}

#[tokio::test]
async fn malformed_native_witness_or_oversized_path_is_not_an_available_empty_source() {
    for (field, value) in [
        ("target_package", Val::Text("another-package".into())),
        ("file_path_bytes", Val::Int(8001)),
        ("event_session_id", Val::Int(-1)),
        ("target_id", Val::Int(-1)),
    ] {
        let mut io = FakeRead::new(vec![Ok(vec![replace(&configuration(), &[(field, value)])])]);
        assert!(ConfiguredTarget::read(&mut io).await.is_err(), "{field}");
        assert_eq!(io.calls.len(), 1);
    }
}

pub(crate) mod owned_fixture;
