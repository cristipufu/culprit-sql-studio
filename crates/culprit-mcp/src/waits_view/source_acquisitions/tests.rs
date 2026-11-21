use super::*;
use culprit_core::{RecordedSourceAcquisition, SourceOverheadEvidence, telemetry::*};
fn report() -> culprit_core::Report {
    serde_json::from_str(include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/mssql-demo.report.json")))
        .unwrap()
}
fn cached() -> HistoricalSourceOverhead {
    HistoricalSourceOverhead::Available {
        manifest_sha256: digest(b"foreign-cached-body"),
        report: Box::new(
            culprit_engine::source_overhead::explain(&SourceOverheadEvidence {
                capture: CaptureIdentity { observation_id: "cached-opaque".into(), revision: None },
                engine: None,
                source: None,
                status: CollectionStatus::Complete,
                started_at: "2020-01-01T00:00:00Z".into(),
                finished_at: Some("2020-01-01T00:00:01Z".into()),
                source_queries: Some(7),
                read_metrics: None,
            })
            .unwrap(),
        ),
    }
}
#[test]
fn saved_metadata_projection_reinterprets_original_markers_without_trusting_cached_prose_or_current_scan_identity() {
    let mut report = report();
    report.source_acquisitions = vec![cached()];
    report.snapshot.operations.source_acquisitions = vec![RecordedSourceAcquisition::Unavailable {
        reason: culprit_core::message!("view.source_overhead.initial_missing"),
    }];
    let mut value = json!({});
    attach(&mut value, &report);
    assert_eq!(value["source_acquisitions"][0]["kind"], "unavailable");
    assert_eq!(value["source_acquisitions"][0]["reason"], culprit_core::t!("view.source_overhead.initial_missing"));
    assert!(value["source_acquisitions"][0].get("report").is_none());
    assert!(!value.to_string().contains("2020-01-01"));
}
#[test]
fn cached_available_metadata_without_original_cannot_supply_measurements_and_unrequested_history_is_absent() {
    let mut report = report();
    report.snapshot.operations.source_acquisitions.clear();
    report.source_acquisitions = vec![cached()];
    let mut value = json!({});
    attach(&mut value, &report);
    assert_eq!(value["source_acquisitions"][0]["kind"], "unavailable");
    assert_eq!(value["source_acquisitions"][0]["reason"], culprit_core::t!("view.source_overhead.invalid_saved"));
    assert!(!value.to_string().contains("cached-opaque"));
    report.source_acquisitions.clear();
    let mut value = json!({});
    attach(&mut value, &report);
    assert!(value.get("source_acquisitions").is_none());
}
