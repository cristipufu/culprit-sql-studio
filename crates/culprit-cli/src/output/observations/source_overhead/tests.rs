use super::*;
use culprit_core::{
    Engine, SourceOverheadEvidence,
    telemetry::{CaptureIdentity, CollectionStatus, CollectionStop},
};

fn report() -> SourceOverheadReport {
    culprit_checks::source_overhead::explain(&SourceOverheadEvidence {
        capture: CaptureIdentity { observation_id: "capture".into(), revision: None },
        engine: Some(Engine::Postgres),
        status: CollectionStatus::Complete,
        started_at: "2026-11-17T12:00:00.000000001Z".into(),
        finished_at: Some("2026-11-17T12:00:05.000000009Z".into()),
        source_queries: Some(culprit_collector::analytical::test_support::SOURCE_OVERHEAD_QUERY_COUNT),
        read_metrics: Some(culprit_collector::analytical::test_support::source_overhead_read_metrics()),
    })
    .unwrap()
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
    let mut evidence = report().evidence;
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
