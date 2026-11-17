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
