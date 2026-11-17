use super::super::tests::{explain_native as explain, native, observation};
use culprit_core::{
    SourceOutcome, SourceState, t,
    telemetry::{MssqlCompilationStage, MssqlCompilationTimings},
};

#[test]
fn phase_metrics_preserve_exact_last_native_values_and_do_not_change_advice() {
    let mut point = observation("9007199254740993");
    let before = explain(&[&point], None).unwrap();
    native(&mut point).timings = MssqlCompilationTimings {
        last_duration_us: Some("9007199254740993".into()),
        binding: MssqlCompilationStage {
            average_duration_us: Some(12.5),
            last_duration_us: Some("0".into()),
            average_cpu_us: None,
            last_cpu_us: Some("9007199254740994".into()),
        },
        optimization: MssqlCompilationStage {
            average_duration_us: None,
            last_duration_us: Some("90".into()),
            average_cpu_us: Some(0.0),
            last_cpu_us: Some("13".into()),
        },
    };
    let after = explain(&[&point], None).unwrap();
    let entry = &after.entries[0];
    for (key, text) in [
        ("view.compilation.metric.last_duration", "9007199254740993 µs"),
        ("view.compilation.metric.average_bind_duration", "12.5 µs"),
        ("view.compilation.metric.last_bind_duration", "0 µs"),
        ("view.compilation.metric.last_bind_cpu", "9007199254740994 µs"),
        ("view.compilation.metric.average_optimize_cpu", "0 µs"),
    ] {
        let metric = entry.metrics.iter().find(|metric| metric.is(key)).unwrap();
        assert_eq!(metric.text.as_deref(), Some(text));
    }
    assert_eq!(
        entry
            .metrics
            .iter()
            .find(|metric| metric.is("view.compilation.metric.average_bind_cpu"))
            .unwrap()
            .text
            .as_deref(),
        Some(t!("view.compilation.timing_unknown").as_str())
    );
    assert_eq!(entry.investigation, before.entries[0].investigation);
    assert_eq!(entry.verify, before.entries[0].verify);
    assert_eq!(entry.explanation, before.entries[0].explanation);
    assert!(after.limitations.contains(&t!("view.compilation.limit.stage_timings")));
    for metric in &entry.metrics {
        assert!(!metric.key.contains("total_cpu") && !metric.key.contains("concurrent"));
    }
}

#[test]
fn partial_population_and_unknown_stages_remain_explicit_without_invalid_zero_repair() {
    let mut point = observation("1");
    let source =
        SourceOutcome { source: point.source.clone(), state: SourceState::Partial, reason: None, reason_message: None };
    let partial = explain(&[&point], Some(&source)).unwrap();
    assert_eq!(partial.source_state, Some(SourceState::Partial));
    assert_eq!(
        partial.entries[0]
            .metrics
            .iter()
            .filter(|metric| metric.text.as_deref() == Some(t!("view.compilation.timing_unknown").as_str()))
            .count(),
        9
    );
    native(&mut point).timings.optimization.last_cpu_us = Some("-1".into());
    assert!(explain(&[&point], Some(&source)).is_err());
}
