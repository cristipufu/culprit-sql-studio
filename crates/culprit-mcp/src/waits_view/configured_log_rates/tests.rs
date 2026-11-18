use super::*;
use culprit_core::telemetry::*;

fn report() -> Report {
    let mut report: Report = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../fixtures/mssql-demo.report.json"
    )))
    .unwrap();
    let mut target =
        culprit_core::DatabaseIdentity::server(culprit_core::Engine::Mssql, "app", "private-host-canary", 1433, None);
    target.incarnation = Some("private-native-incarnation".into());
    report.snapshot.server.database = "app".into();
    report.snapshot.bind_identity(target.clone());
    report.snapshot.pressure.get_or_insert_with(Default::default).configured_log_rates =
        Some(Box::new(ConfiguredLogRateRead {
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
                database_incarnation: "private-native-incarnation".into(),
                workload_group_id: None,
                workload_group_bytes_per_second: Some("9007199254740993".into()),
                resource_pool_bytes_per_second: Some("0".into()),
                instance_bytes_per_second: None,
            })),
        }));
    report.configured_log_rates = culprit_engine::explain_configured_log_rates(&report.snapshot).unwrap().map(Box::new);
    report
}

#[test]
fn compact_native_summary_reanalyzes_raw_configuration_and_excludes_private_namespace_and_stale_reports() {
    let mut report = report();
    report.configured_log_rates.as_mut().unwrap().entries[0].metric.text = Some("stale-derived-secret-canary".into());
    let mut response = json!({});
    attach(&mut response, &report);
    let body = serde_json::to_string(&response).unwrap();
    for secret in ["private-host-canary", "private-native-incarnation", "stale-derived-secret-canary"] {
        assert!(!body.contains(secret));
    }
    assert!(body.contains("9007199254740993"));
    assert_eq!(response["configured_log_rates"]["entries"].as_array().unwrap().len(), 3);
    assert_eq!(response["configured_log_rates"]["native_rows"], 1);
}

#[test]
fn invalid_sharing_marker_remains_unavailable_and_cannot_reuse_a_stale_positive_explanation() {
    let mut report = report();
    report
        .snapshot
        .pressure
        .as_mut()
        .unwrap()
        .configured_log_rates
        .as_mut()
        .unwrap()
        .observation
        .as_mut()
        .unwrap()
        .instance_bytes_per_second = Some("-1".into());
    report.snapshot = culprit_core::redact::redact_snapshot(&report.snapshot);
    let mut response = json!({});
    attach(&mut response, &report);
    assert!(response.get("configured_log_rates").is_none());
    assert!(response.get("configured_log_rates_unavailable").is_some());
    assert!(!response.to_string().contains("9007199254740993"));
}
