use super::*;
use crate::Engine;
use std::time::Duration;

fn recorded(outcomes: &[ReadTaskOutcome]) -> ReadWorkMetrics {
    let recorder = ReadMetricsRecorder::new(Engine::Mssql);
    for outcome in outcomes {
        let id = recorder.begin("SELECT N'private-stop-cause-canary'");
        recorder.finish(id.as_deref(), Duration::from_millis(1), &ReadProgress::default(), outcome);
    }
    recorder.snapshot().unwrap()
}

fn aggregate() -> ReadWorkMetrics {
    recorded(&[
        ReadTaskOutcome { unconfirmed: true, ..ReadTaskOutcome::stopped(CollectionStop::ByteLimit) },
        ReadTaskOutcome::stopped(CollectionStop::ByteLimit),
        ReadTaskOutcome::stopped(CollectionStop::RowLimit),
        ReadTaskOutcome::stopped(CollectionStop::TimedOut),
        ReadTaskOutcome::stopped(CollectionStop::Cancelled),
        ReadTaskOutcome { failed: true, unconfirmed: true, ..Default::default() },
        ReadTaskOutcome { failed: true, timed_out: true, ..Default::default() },
        ReadTaskOutcome::default(),
    ])
}

#[test]
fn original_stop_requests_aggregate_separately_from_final_settlement_and_native_timeout_errors() {
    let metrics = aggregate();
    assert!(metrics.is_valid());
    let metric = &metrics.templates[0];
    assert_eq!((metric.completed_tasks, metric.failures), (8, 7));
    assert_eq!((metric.timeouts, metric.cancellations, metric.unconfirmed_cancellations), (2, 1, 2));
    assert_eq!(
        metric.stop_causes,
        [
            ReadStopCount { reason: CollectionStop::ByteLimit, tasks: 2 },
            ReadStopCount { reason: CollectionStop::RowLimit, tasks: 1 },
            ReadStopCount { reason: CollectionStop::TimedOut, tasks: 1 },
            ReadStopCount { reason: CollectionStop::Cancelled, tasks: 1 },
        ]
    );
    assert!(metrics.has_unconfirmed_work());
}

#[test]
fn normal_completion_and_unknown_original_causes_never_invent_stop_requests() {
    for outcome in [
        ReadTaskOutcome::default(),
        ReadTaskOutcome { failed: true, ..Default::default() },
        ReadTaskOutcome { failed: true, timed_out: true, ..Default::default() },
        ReadTaskOutcome { unconfirmed: true, ..ReadTaskOutcome::stopped(CollectionStop::CancellationUnconfirmed) },
    ] {
        let metrics = recorded(&[outcome]);
        assert!(metrics.is_valid());
        assert!(metrics.templates[0].stop_causes.is_empty());
    }
}

#[test]
fn a_known_stop_request_does_not_establish_quiescence_or_clear_unconfirmed_work() {
    let mut metrics = recorded(&[ReadTaskOutcome::stopped(CollectionStop::ByteLimit)]);
    assert!(!metrics.has_unconfirmed_work());
    metrics.active_tasks = 1;
    assert!(metrics.has_unconfirmed_work());
    metrics.active_tasks = 0;
    metrics.templates[0].unconfirmed_cancellations = 1;
    assert!(metrics.has_unconfirmed_work());
    assert_eq!(metrics.templates[0].stop_causes[0].reason, CollectionStop::ByteLimit);
}

#[test]
fn malformed_original_stop_counts_cannot_authorize_a_valid_summary() {
    type Alter = fn(&mut ReadQueryMetrics);
    let cases: &[(&str, Alter)] = &[
        ("duplicate", |metric| metric.stop_causes.push(metric.stop_causes[0].clone())),
        ("zero", |metric| metric.stop_causes[0].tasks = 0),
        ("pending", |metric| metric.stop_causes[0].reason = CollectionStop::Pending),
        ("settlement", |metric| metric.stop_causes[0].reason = CollectionStop::CancellationUnconfirmed),
        ("too many failures", |metric| metric.stop_causes[0].tasks = metric.failures + 1),
        ("sum exceeds failures", |metric| metric.failures = 4),
        ("wide sum", |metric| {
            metric.completed_tasks = u32::MAX;
            metric.failures = u32::MAX;
            metric.stop_causes[0].tasks = u32::MAX;
            metric.stop_causes[1].tasks = u32::MAX;
        }),
        ("timeout without counter", |metric| metric.timeouts = 0),
        ("caller cancellation without counter", |metric| metric.cancellations = 0),
    ];
    for (name, alter) in cases {
        let mut metrics = aggregate();
        alter(&mut metrics.templates[0]);
        assert!(!metrics.is_valid(), "{name}");
        assert!(metrics.summary(None).is_err(), "{name}");
    }
}

#[test]
fn required_typed_causes_reject_missing_fields_and_unknown_reasons() {
    let metrics = aggregate();
    let mut raw = serde_json::to_value(&metrics).unwrap();
    raw["templates"][0].as_object_mut().unwrap().remove("stop_causes");
    assert!(serde_json::from_value::<ReadWorkMetrics>(raw).is_err());
    let mut raw = serde_json::to_value(&metrics).unwrap();
    raw["templates"][0]["stop_causes"][0]["reason"] = serde_json::json!("private-native-error-text");
    assert!(serde_json::from_value::<ReadWorkMetrics>(raw).is_err());
}

#[test]
fn public_summary_keeps_exact_stop_causes_without_source_sql_or_a_new_settlement_claim() {
    let metrics = aggregate();
    let summary = metrics.summary(None).unwrap();
    assert_eq!(summary.templates[0].stop_causes, metrics.templates[0].stop_causes);
    assert_eq!(summary.templates[0].unconfirmed_cancellations, 2);
    let public = serde_json::to_string(&summary).unwrap();
    assert!(public.contains("byte-limit"));
    assert!(!public.contains("private-stop-cause-canary"));
    assert!(matches!(
        summary.templates[0].statement,
        DefinitionPayload::Unavailable { reason: PayloadUnavailable::Restricted }
    ));
}
