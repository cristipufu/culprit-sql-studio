use super::*;
use culprit_core::{
    Engine, SourceOverheadEvidence,
    telemetry::{CaptureIdentity, CollectionStatus, CollectionStop},
};

fn evidence() -> SourceOverheadEvidence {
    SourceOverheadEvidence {
        capture: CaptureIdentity { observation_id: "capture".into(), revision: None },
        engine: Some(Engine::Postgres),
        source: None,
        status: CollectionStatus::Complete,
        started_at: "2026-11-17T12:00:00.000000001Z".into(),
        finished_at: Some("2026-11-17T12:00:05.000000009Z".into()),
        source_queries: Some(culprit_collector::analytical::test_support::SOURCE_OVERHEAD_QUERY_COUNT),
        read_metrics: Some(culprit_collector::analytical::test_support::source_overhead_read_metrics()),
    }
}
fn report() -> SourceOverheadReport {
    culprit_checks::source_overhead::explain(&evidence()).unwrap()
}

#[test]
fn text_shows_only_top_five_template_ids_with_exact_counts_and_all_limits() {
    let report = report();
    let text = source_overhead(&report);
    let metrics = report.evidence.read_metrics.as_ref().unwrap();
    for (rank, index) in report.template_order.iter().enumerate() {
        assert_eq!(
            text.contains(&metrics.templates[*index as usize].template_id),
            rank < TEMPLATE_LIMIT,
            "rank {rank}"
        );
    }
    assert!(text.contains("18446744073709551615"));
    assert!(text.contains("9007199254740993"));
    assert!(text.contains("2026-11-17T12:00:00.000000001Z"));
    assert!(text.contains("2026-11-17T12:00:05.000000009Z"));
    assert!(!text.contains("overhead_preview_canary"));
    for limitation in &report.limitations {
        assert!(text.contains(limitation));
    }
    let json = serde_json::to_value(&report).unwrap();
    assert_eq!(json["evidence"]["read_metrics"]["templates"].as_array().unwrap().len(), 32);
    assert_eq!(json["evidence"]["source_queries"], 100);
    assert!(
        json["evidence"]["read_metrics"]["templates"]
            .as_array()
            .unwrap()
            .iter()
            .all(|metric| metric["statement"]["text"].is_null())
    );
}

#[test]
fn missing_recording_and_unfinished_local_state_never_become_zero_cost_or_success() {
    let mut evidence = evidence();
    evidence.status = CollectionStatus::Incomplete(CollectionStop::TimedOut);
    evidence.finished_at = None;
    evidence.source_queries = None;
    evidence.read_metrics = None;
    let report = culprit_checks::source_overhead::explain(&evidence).unwrap();
    let text = source_overhead(&report);
    assert!(text.contains(&t!("view.source_overhead.summary.not_recorded")));
    assert!(text.contains(&t!("view.source_overhead.limit.incomplete")));
    assert!(text.contains(&t!("view.source_overhead.limit.total")));
    assert!(!text.contains("9007199254740993"));
}

#[test]
fn native_source_session_output_keeps_boundary_limits_and_never_renders_private_scope_or_sql() {
    let mut input = evidence();
    input.engine = None;
    let metrics = culprit_collector::analytical::test_support::native_source_overhead_read_metrics();
    input.read_metrics = Some(metrics.clone());
    let report = culprit_checks::source_overhead::explain(&input).unwrap();
    let text = source_overhead(&report);
    let native = report.native_interval.as_ref().unwrap();
    assert!(text.contains(&native.summary));
    for detail in native.measurements.iter().chain(&native.limitations) {
        assert!(text.contains(detail));
    }
    let public = serde_json::to_value(&report).unwrap();
    culprit_collector::analytical::test_support::assert_native_source_overhead_summary(
        &public["evidence"]["read_metrics"],
        &metrics,
    );
    for private in ["native-cost-reader", "2020-01-01T11:30:00.0030000", "overhead_preview_canary"] {
        assert!(!text.contains(private), "{private}");
    }
    assert_eq!(report.evidence.source_queries, Some(100));
    assert_eq!(report.evidence.read_metrics.as_ref().unwrap().templates.len(), 32);
}

#[test]
fn source_acquisition_text_and_json_keep_login_failure_reuse_and_query_failure_as_separate_observations() {
    let mut evidence = evidence();
    evidence.started_at = "2020-01-01T12:00:00Z".into();
    evidence.finished_at = Some("2020-01-01T12:00:01Z".into());
    evidence.source = None;
    evidence.engine = None;
    evidence.read_metrics =
        Some(culprit_collector::analytical::test_support::connection_source_overhead_read_metrics());
    let report = culprit_checks::source_overhead::explain(&evidence).unwrap();
    let text = source_overhead(&report);
    let view = report.connection_work.as_ref().unwrap();
    assert!(text.contains(&view.summary));
    for line in view.observations.iter().chain(&view.limitations) {
        assert!(text.contains(line));
    }
    let public = serde_json::to_value(&report).unwrap();
    let attempts = &public["evidence"]["read_metrics"]["connection_work"]["attempts"];
    assert_eq!(attempts[0]["reader_generation"], serde_json::Value::Null);
    assert_eq!(attempts[0]["phases"][1]["outcome"]["native_code"], 18456);
    assert_eq!(attempts[1]["connection_use"], "authenticated-reuse");
    assert_eq!(attempts[1]["phases"][1]["outcome"]["code"], "permission_denied");
    assert!(public["evidence"]["engine"].is_null());
    assert!(public["evidence"]["read_metrics"]["native_session_work"].is_null());
}

#[test]
fn bounded_saved_history_uses_the_same_canonical_observations_and_marks_withdrawn_or_unpresented_originals() {
    let mut view = report();
    view.limitations.push(t!("view.source_overhead.limit.historical"));
    let first = culprit_core::HistoricalSourceOverhead::Available {
        manifest_sha256: "private-archive-digest".into(),
        report: Box::new(view),
    };
    let withheld =
        culprit_core::HistoricalSourceOverhead::Unavailable { reason: t!("view.source_overhead.sharing_withheld") };
    let text = historical_source_overhead(&[first.clone(), withheld.clone(), withheld.clone(), first]);
    assert!(text.contains("9007199254740993"));
    assert!(text.contains(&t!("view.source_overhead.limit.historical")));
    assert!(text.contains(&t!("view.source_overhead.sharing_withheld")));
    assert!(text.contains(&t!("view.source_overhead.history_overflow", shown = "3", recorded = "4")));
    assert!(!text.contains("private-archive-digest"));
    assert!(!text.contains("overhead_preview_canary"));
}
#[test]
fn withdrawn_saved_metadata_has_no_borrowed_total_clock_or_current_identity() {
    let text = historical_source_overhead(&[culprit_core::HistoricalSourceOverhead::Unavailable {
        reason: t!("view.source_overhead.sharing_withheld"),
    }]);
    assert!(text.contains(&t!("view.source_overhead.sharing_withheld")));
    assert!(!text.contains("2026-11-17"));
    assert!(!text.contains("9007199254740993"));
}
