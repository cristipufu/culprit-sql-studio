use super::*;
use culprit_core::{SourceState, telemetry::*};
fn report() -> SuspectPageHistoryReport {
    SuspectPageHistoryReport {
        context: Some(ObservationReadContext::Native {
            context: Box::new(NativeReadContext {
                target: culprit_core::DatabaseIdentity::server(
                    culprit_core::Engine::Mssql,
                    "shop",
                    "private-host",
                    1433,
                    None,
                )
                .into(),
                partition: SourcePartition::InstanceLocal {
                    member: Some(SharedResourceIdentity {
                        engine: culprit_core::Engine::Mssql,
                        kind: SharedResourceKind::Instance,
                        namespace: NativeIdentity::new("private-machine"),
                        native: NativeIdentity::new("private-member"),
                    }),
                    epoch: Some("private-process".into()),
                },
                observed_from: "2020-01-01T00:00:00Z".into(),
                observed_until: "2020-01-01T00:00:01Z".into(),
                database_writable: Some(true),
            }),
        }),
        native_source: None,
        source_state: Some(SourceState::Collected),
        source_reason: None,
        read_complete: Some(true),
        database: Some("shop".into()),
        native_database_id: Some("7".into()),
        observed_at: Some("2020-01-01T00:00:00.5Z".into()),
        active_rows: Some("0".into()),
        retained_rows: Some("3".into()),
        last_reported_time_without_offset: None,
        title: "history".into(),
        summary: "recorded".into(),
        metrics: vec![],
        limitations: vec!["retained only".into()],
    }
}
#[test]
fn compact_history_retains_repaired_only_and_unknown_states_without_source_proof() {
    let report = report();
    let shown = value(&report);
    assert_eq!(shown["active_rows"], json!("0"));
    assert_eq!(shown["retained_rows"], json!("3"));
    assert!(shown["last_reported_time_without_offset"].is_null());
    for private in [
        "private-host",
        "private-machine",
        "private-member",
        "private-process",
        "partition",
        "qualifiers",
        "manifest_sha256",
    ] {
        assert!(!shown.to_string().contains(private));
    }
    let mut unavailable = report;
    unavailable.source_state = Some(SourceState::Unavailable);
    unavailable.active_rows = None;
    unavailable.retained_rows = None;
    let value = value(&unavailable);
    assert!(value["active_rows"].is_null());
    assert!(value["retained_rows"].is_null());
}

#[test]
fn health_drilldown_preserves_positive_retained_history_without_a_checksum_event_finding() {
    let mut scan: culprit_core::Report = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../fixtures/mssql-demo.report.json"
    )))
    .unwrap();
    scan.findings.clear();
    let mut history = report();
    history.active_rows = Some("2".into());
    history.retained_rows = Some("4".into());
    scan.suspect_pages = Some(Box::new(history.clone()));
    let shown = crate::waits_view::report(&scan);
    assert_eq!(shown["suspect_pages"], value(&history));
    assert_eq!(shown["suspect_pages"]["active_rows"], "2");
    assert_eq!(shown["suspect_pages"]["retained_rows"], "4");
    assert_eq!(shown["suspect_pages"]["read_bounds"]["observed_from"], "2020-01-01T00:00:00Z");
    assert_eq!(shown["suspect_pages"]["limitations"], json!(["retained only"]));
}
