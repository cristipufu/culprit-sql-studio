use super::*;
use culprit_core::{DatabaseIdentity, Engine, telemetry::*};

fn read() -> ConfiguredLogRateRead {
    let mut target = DatabaseIdentity::server(Engine::Mssql, "app", "fixture.invalid", 1433, None);
    target.incarnation = Some("logical-guid".into());
    ConfiguredLogRateRead {
        context: NativeReadContext {
            target: target.into(),
            partition: SourcePartition::Target,
            observed_from: "2020-01-01T12:00:00Z".into(),
            observed_until: "2020-01-01T12:00:01Z".into(),
            database_writable: None,
        },
        source: StateSourceCoverage::observed(CONFIGURED_LOG_RATES_SOURCE.into(), 1, false),
        observation: Some(Box::new(ConfiguredLogRateLimits {
            observed_at: Some(canonical_time("2020-01-01T12:00:00Z").unwrap()),
            database_id: "7".into(),
            database_incarnation: "logical-guid".into(),
            workload_group_id: None,
            workload_group_bytes_per_second: Some("9007199254740993".into()),
            resource_pool_bytes_per_second: Some("0".into()),
            instance_bytes_per_second: None,
        })),
    }
}

#[test]
fn exact_three_scopes_are_configuration_context_without_inferred_pressure_or_shared_ownership() {
    let report = explain(&read(), true).unwrap();
    assert_eq!(report.entries.len(), 3);
    assert_eq!(report.entries[0].bytes_per_second.as_deref(), Some("9007199254740993"));
    assert!(report.entries[0].metric.text.as_ref().unwrap().contains("9007199254740993"));
    assert_eq!(report.entries[1].bytes_per_second.as_deref(), Some("0"));
    assert!(report.entries[2].bytes_per_second.is_none());
    assert_eq!(report.entries[1].scope, ConfiguredLogRateScope::UserResourcePool);
    assert_eq!(report.entries[2].scope, ConfiguredLogRateScope::DatabaseEngineInstance);
    assert!(report.limitations.contains(&t!("view.configured_log_rates.limit.components")));
    assert!(report.limitations.contains(&t!("view.configured_log_rates.limit.identity")));
}

#[test]
fn missing_archived_body_unavailable_and_null_values_never_become_healthy_zero() {
    let mut missing = read();
    missing.observation = None;
    let report = explain(&missing, false).unwrap();
    assert_eq!(report.native_rows, 1);
    assert_eq!(report.observed_rows, 0);
    assert!(report.entries.is_empty());
    assert!(explain(&missing, true).is_err());
    missing.source = StateSourceCoverage::unavailable(
        CONFIGURED_LOG_RATES_SOURCE.into(),
        culprit_core::ErrorCode::PermissionDenied,
        None,
    );
    let report = explain(&missing, true).unwrap();
    assert_eq!(report.source_state, SourceState::Unavailable);
    assert!(report.entries.is_empty());
    let mut unknown = read();
    let point = unknown.observation.as_mut().unwrap();
    point.workload_group_bytes_per_second = None;
    point.resource_pool_bytes_per_second = None;
    let report = explain(&unknown, true).unwrap();
    assert!(report.entries.iter().all(|entry| entry.bytes_per_second.is_none()));
}

#[test]
fn invalid_clock_incarnation_and_numeric_payloads_are_rejected_before_rendering() {
    for field in ["clock", "incarnation", "value"] {
        let mut read = read();
        let point = read.observation.as_mut().unwrap();
        match field {
            "clock" => point.observed_at = Some(canonical_time("2020-01-01T12:00:02Z").unwrap()),
            "incarnation" => point.database_incarnation = "another-original-database".into(),
            _ => point.instance_bytes_per_second = Some("-1".into()),
        }
        assert!(explain(&read, true).is_err());
    }
}

#[test]
fn outer_namespace_uses_shared_incarnation_comparison_and_keeps_unknown_display_qualified() {
    let original = read();
    let mut snapshot: culprit_core::Snapshot =
        serde_json::from_str(include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/mssql-demo.json")))
            .unwrap();
    snapshot.server.database = "app".into();
    snapshot.bind_identity(original.context.target.database().unwrap().clone());
    snapshot.pressure.get_or_insert_with(Default::default).configured_log_rates = Some(Box::new(original.clone()));
    assert!(explain_snapshot(&snapshot).unwrap().is_some());
    let mut alias = DatabaseIdentity::server(Engine::Mssql, "app", "FIXTURE.INVALID.", 1433, None);
    alias.incarnation = original.context.target.database().unwrap().incarnation.clone();
    snapshot.bind_identity(alias);
    assert!(explain_snapshot(&snapshot).unwrap().is_some(), "the shared identity policy normalizes endpoint spelling");
    let mut another_endpoint =
        DatabaseIdentity::server(Engine::Mssql, "app", "another-native-route.invalid", 1433, None);
    another_endpoint.incarnation = original.context.target.database().unwrap().incarnation.clone();
    snapshot.bind_identity(another_endpoint);
    assert!(explain_snapshot(&snapshot).is_err(), "a matching incarnation does not override a foreign endpoint");
    let mut foreign = original.context.target.database().unwrap().clone();
    foreign.incarnation = Some("recreated-native-database".into());
    snapshot.bind_identity(foreign);
    assert!(explain_snapshot(&snapshot).is_err());
    let mut unknown = original.context.target.database().unwrap().clone();
    unknown.incarnation = None;
    snapshot.bind_identity(unknown);
    let report = explain_snapshot(&snapshot).unwrap().unwrap();
    assert_eq!(report.entries[0].bytes_per_second.as_deref(), Some("9007199254740993"));
    assert!(report.limitations.contains(&t!("view.observations.limit.namespace")));
}

#[test]
fn absent_native_point_clock_is_not_filled_from_read_bounds_or_used_to_discard_accessible_values() {
    let mut read = read();
    read.observation.as_mut().unwrap().observed_at = None;
    let report = explain(&read, true).unwrap();
    assert!(report.observed_at.is_none());
    assert_eq!(report.entries[0].bytes_per_second.as_deref(), Some("9007199254740993"));
    assert!(report.limitations.contains(&t!("view.configured_log_rates.limit.clock")));
    read.context.observed_until = "2019-12-31T12:00:00Z".into();
    assert!(explain(&read, true).is_err(), "missing point time does not erase ordering of the independent read bounds");
}
