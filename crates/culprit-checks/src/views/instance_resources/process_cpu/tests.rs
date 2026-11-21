use super::*;

fn sample() -> NativeCounterSet {
    serde_json::from_str(include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/process-cpu.sample.json")))
        .unwrap()
}

#[test]
fn totals_are_exact_point_fields_and_native_zero_is_distinct_from_missing() {
    let sample = sample();
    let mut report = empty(SourceState::Collected);
    fill(&mut report, &[&sample]).unwrap();
    assert_eq!(report.observed_at.as_deref(), Some("2020-01-01T12:00:00.5Z"));
    assert_eq!(report.fields[1].value.as_deref(), Some("9007199254740993"));
    assert_eq!(report.fields[2].value.as_deref(), Some("0"));
    assert!(!report.fields.iter().any(|field| field.key.contains("percent")));
    let mut missing = sample.clone();
    missing.counters.clear();
    fill(&mut report, &[&missing]).unwrap();
    assert!(report.fields[1..3].iter().all(|field| field.value.is_none()));
}

#[test]
fn incomplete_or_absent_point_retains_qualifications_and_never_becomes_zero_usage() {
    let mut report = empty(SourceState::Partial);
    fill(&mut report, &[]).unwrap();
    qualify(&mut report);
    assert!(report.observed_at.is_none() && report.fields.is_empty());
    assert!(report.limitations.contains(&t!("view.process_cpu.limit.partial")));
    assert!(report.limitations.contains(&t!("view.process_cpu.limit.unavailable")));
    assert_eq!(report.summary, t!("view.process_cpu.summary.unavailable"));
}

#[test]
fn two_original_points_are_not_summed_or_compared_as_one_cpu_population() {
    let first = sample();
    let mut next = first.clone();
    next.properties.insert("sample_ms_ticks".into(), "501".into());
    next.properties.insert("observed_at".into(), "2020-01-01T12:00:00.6Z".into());
    assert!(fill(&mut empty(SourceState::Collected), &[&first, &next]).is_err());
}
