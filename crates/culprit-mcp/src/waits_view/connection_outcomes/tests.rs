use super::*;
use culprit_core::{
    ConnectionOutcomeObservation, ConnectionOutcomeSnapshot, DatabaseIdentity, Engine, MeasurementScope, telemetry::*,
};
use std::collections::BTreeMap;

fn report_with_rows(rows: usize) -> Box<Report> {
    let mut report: Box<Report> =
        serde_json::from_str(include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/pg-demo.report.json")))
            .unwrap();
    let target =
        DatabaseIdentity::server(Engine::Postgres, "application", "private-connection-host.invalid", 5432, None);
    report.snapshot.server.database = "application".into();
    report.snapshot.bind_identity(target.clone());
    report.snapshot.pressure.get_or_insert_default().connection_outcomes = Some(Box::new(ConnectionOutcomeSnapshot {
        target: target.into(),
        context: ObservationReadContext::Collector {
            observed_from: "2026-01-01T00:00:00Z".into(),
            observed_until: "2026-01-01T00:00:01Z".into(),
        },
        sources: vec![StateSourceCoverage::observed("pg_stat_database".into(), rows as u32, true)],
        observations: (0..rows)
            .map(|index| ConnectionOutcomeObservation {
                source: "pg_stat_database".into(),
                counters: NativeCounterSet {
                    identity: NativeIdentity::new("pg_stat_database")
                        .qualified("dbid", "16384")
                        .qualified("private-native-member", "private-member-token")
                        .qualified("retained-native-row", index.to_string()),
                    scope: MeasurementScope::Database,
                    properties: BTreeMap::from([
                        ("datname".into(), "application".into()),
                        ("has_sessions".into(), "true".into()),
                    ]),
                    counters: vec![NativeCounterSample {
                        key: "sessions".into(),
                        value: MeasurementValue::Integer("9007199254740993".into()),
                        unit: CounterUnit::Count,
                        nature: CounterNature::Cumulative,
                        scope: MeasurementScope::Database,
                        reset: ResetMarker::Generation("private-native-reset".into()),
                        native_object: "pg_stat_database".into(),
                        native_name: "sessions".into(),
                        native_instance: None,
                        native_type: None,
                    }],
                },
            })
            .collect(),
    }));
    report
}

#[test]
fn compact_output_rewords_current_raw_facts_and_keeps_exact_counts_coverage_and_a_real_entry_limit() {
    let mut data = report_with_rows(ENTRY_LIMIT + 3);
    let mut stale = culprit_engine::explain_connection_outcomes(&data.snapshot).unwrap().unwrap();
    stale.title = "stale-language-canary".into();
    stale.entries[0].metrics[0].display_value = Some("wrong-rounded-value".into());
    data.connection_outcomes = Some(Box::new(stale));
    let mut response = json!({});
    attach(&mut response, &data);
    let explained = &response["connection_outcomes"];
    assert_eq!(explained["native_rows"], ENTRY_LIMIT + 3);
    assert_eq!(explained["mapped_rows"], ENTRY_LIMIT + 3);
    assert_eq!(explained["entries"].as_array().unwrap().len(), ENTRY_LIMIT);
    assert_eq!(explained["entries_limited"], true);
    assert_eq!(explained["entries"][0]["metrics"][0]["value"]["value"], "9007199254740993");
    let text = serde_json::to_string(explained).unwrap();
    for private in [
        "private-connection-host",
        "private-member-token",
        "private-native-reset",
        "stale-language-canary",
        "wrong-rounded-value",
    ] {
        assert!(!text.contains(private), "{private}");
    }
    assert!(explained.get("target").is_none() && explained.get("observations").is_none());
    assert!(explained["entries"][0].get("session").is_none() && explained["entries"][0].get("query").is_none());
}

#[test]
fn unknown_outer_identity_preserves_qualified_values_but_known_conflicts_and_malformed_raw_data_refuse() {
    let mut data = report_with_rows(1);
    data.snapshot.identity = None;
    let mut response = json!({});
    attach(&mut response, &data);
    assert_eq!(response["connection_outcomes"]["observed_rows"], 1);
    assert!(
        response["connection_outcomes"]["limitations"]
            .as_array()
            .unwrap()
            .contains(&json!(culprit_core::t!("view.observations.limit.namespace")))
    );
    for change in ["database", "scope", "count"] {
        let mut data = report_with_rows(1);
        match change {
            "database" => data.snapshot.server.database = "another".into(),
            "scope" => {
                data.snapshot.pressure.as_mut().unwrap().connection_outcomes.as_mut().unwrap().observations[0]
                    .counters
                    .scope = MeasurementScope::Server
            }
            "count" => {
                data.snapshot.pressure.as_mut().unwrap().connection_outcomes.as_mut().unwrap().sources[0].records = 0
            }
            _ => unreachable!(),
        }
        let mut response = json!({});
        attach(&mut response, &data);
        assert!(response.get("connection_outcomes").is_none());
        assert!(response.get("connection_outcomes_unavailable").is_some());
    }
}

#[test]
fn unavailable_native_source_is_explicit_without_counterfeit_zero_or_stale_report_fallback() {
    let mut data = report_with_rows(0);
    data.snapshot.pressure.as_mut().unwrap().connection_outcomes.as_mut().unwrap().sources[0] =
        StateSourceCoverage::unavailable("pg_stat_database".into(), culprit_core::ErrorCode::PermissionDenied, None);
    let mut response = json!({});
    attach(&mut response, &data);
    assert_eq!(response["connection_outcomes"]["sources"][0]["state"], "unavailable");
    assert_eq!(response["connection_outcomes"]["entries"], json!([]));
    data.snapshot.pressure.as_mut().unwrap().connection_outcomes = None;
    let mut response = json!({});
    attach(&mut response, &data);
    assert!(response.get("connection_outcomes").is_none() && response.get("connection_outcomes_unavailable").is_none());
}

#[test]
fn sharing_and_file_reanalysis_preserve_a_withheld_carrier_as_an_explicit_mcp_gap() {
    struct TestDirectory(std::path::PathBuf);
    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let directory = std::env::temp_dir().join(format!("culprit-mcp-connection-gap-{}-{nonce}", std::process::id()));
    std::fs::create_dir(&directory).unwrap();
    let directory = TestDirectory(directory);
    let mut original = report_with_rows(1);
    let stale = culprit_engine::explain_connection_outcomes(&original.snapshot).unwrap().unwrap();
    original.snapshot.pressure.as_mut().unwrap().connection_outcomes.as_mut().unwrap().observations[0]
        .counters
        .identity
        .id
        .clear();
    let shared = culprit_core::redact::redact_snapshot(&original.snapshot);
    assert!(shared.pressure.as_ref().unwrap().connection_outcomes.is_none());
    assert!(shared.source_problem(&[culprit_core::source_id::SESSIONS]).is_some());
    let path = directory.0.join("shared-snapshot.json");
    std::fs::write(&path, serde_json::to_vec(&shared).unwrap()).unwrap();
    let workspace = culprit_engine::Workspace::open_in(directory.0.join("workspace"));
    let mut reanalyzed = workspace.analyze_file(&path).unwrap();
    assert!(reanalyzed.connection_outcomes.is_none());
    assert_eq!(
        reanalyzed.snapshot.source_problem(&[culprit_core::source_id::SESSIONS]),
        Some(culprit_core::t!("view.connection_outcomes.invalid"))
    );
    // A stale prepared view cannot replace the explicit gap in the current shared input.
    reanalyzed.connection_outcomes = Some(Box::new(stale));
    let response = crate::waits_view::report(&reanalyzed);
    assert_eq!(response["connection_outcomes_unavailable"], culprit_core::t!("view.connection_outcomes.invalid"));
    assert!(response.get("connection_outcomes").is_none());
    assert!(response["connection_outcomes_unavailable"].is_string());
    for field in ["native_rows", "observed_rows", "mapped_rows", "entries"] {
        assert!(response["connection_outcomes_unavailable"].get(field).is_none());
    }
}
