use super::*;
use culprit_core::{CompilationMemory, ResetMarker, telemetry::*};

fn observation(count: &str) -> OperationalObservation {
    let native = MssqlQueryCompilation {
        identity: NativeQueryIdentity {
            query_id: "42".into(),
            qualifiers: [
                ("context_settings_id".into(), "5".into()),
                ("object_id".into(), "1999".into()),
                ("query_hash".into(), "0123456789ABCDEF".into()),
                ("initial_compile_at".into(), "2020-01-01T06:00:00.1533333Z".into()),
            ]
            .into(),
        },
        compilations: count.into(),
        last_compile_at: "2020-01-01T11:59:00.1533333Z".into(),
        average_duration_us: 1500.5,
        memory: Some(CompilationMemory { average_kb: Some(2048.5), last_kb: Some(1024.0), maximum_kb: Some(16384.0) }),
        seconds_since_first_compile: 21600,
        reset: ResetMarker::Unknown,
    };
    OperationalObservation {
        source: "query compilation fixture".into(),
        key: native.key().unwrap(),
        fact: OperationalFact::MssqlQueryCompilation(Box::new(native)),
    }
}

fn native(row: &mut OperationalObservation) -> &mut MssqlQueryCompilation {
    let OperationalFact::MssqlQueryCompilation(native) = &mut row.fact else { panic!("compilation") };
    native
}

#[test]
fn exact_compilation_counts_and_individual_memory_measurements_remain_query_scoped_history() {
    let row = observation("18446744073709551615");
    let report = explain(&[&row], None).unwrap();
    let entry = &report.entries[0];
    assert_eq!(entry.observation_key, row.key);
    let OperationalFact::MssqlQueryCompilation(original) = &row.fact else { panic!() };
    assert_eq!(entry.identity, original.identity);
    for (metric, expected) in [
        ("view.compilation.metric.count", "18446744073709551615"),
        ("view.compilation.metric.average_duration", "1500.5 µs"),
        ("common.metric.compile_memory_average", "2048.5 KiB"),
        ("common.metric.compile_memory_last", "1024 KiB"),
        ("common.metric.compile_memory_maximum", "16384 KiB"),
    ] {
        assert_eq!(entry.metrics.iter().find(|item| item.is(metric)).unwrap().text.as_deref(), Some(expected));
    }
    assert_eq!(entry.explanation, t!("view.compilation.entry.multiple", count = "18446744073709551615"));
    assert!(report.limitations.contains(&t!("check.planning_time.caveat.compile_memory")));
    assert!(report.limitations.contains(&t!("view.compilation.limit.causes")));
    let json = serde_json::to_value(&report).unwrap();
    assert!(json.get("snapshot").is_none() && json.get("workload").is_none());
    assert!(json["entries"][0].get("memory_total").is_none());
    assert!(json["entries"][0].get("fix_sql").is_none());
}

#[test]
fn zero_single_and_multiple_retained_compiles_do_not_imply_a_recompile_reason() {
    for (count, expected, investigation) in [
        ("0", t!("view.compilation.entry.zero"), t!("view.compilation.investigate.retained")),
        ("1", t!("view.compilation.entry.single"), t!("view.compilation.investigate.retained")),
        ("2", t!("view.compilation.entry.multiple", count = "2"), t!("view.compilation.investigate.multiple")),
    ] {
        let row = observation(count);
        let report = explain(&[&row], None).unwrap();
        assert_eq!(report.entries[0].explanation, expected);
        assert_eq!(report.entries[0].investigation, investigation);
        assert!(!report.entries[0].verify.is_empty());
    }
}

#[test]
fn unknown_memory_zero_memory_and_clock_skew_preserve_different_native_facts() {
    let mut row = observation("2");
    native(&mut row).memory = None;
    let unknown = explain(&[&row], None).unwrap();
    assert_eq!(unknown.entries[0].metrics.len(), 4);
    assert!(unknown.entries[0].explanation.contains(&t!("view.compilation.entry.memory_unknown")));
    native(&mut row).memory = Some(CompilationMemory { last_kb: Some(0.0), ..Default::default() });
    native(&mut row).seconds_since_first_compile = -1;
    let zero = explain(&[&row], None).unwrap();
    assert_eq!(zero.entries[0].metrics.len(), 5);
    assert_eq!(zero.entries[0].metrics.last().unwrap().text.as_deref(), Some("0 KiB"));
    assert!(!zero.entries[0].explanation.contains(&t!("view.compilation.entry.memory_unknown")));
    assert!(zero.limitations.contains(&t!("view.compilation.limit.clock")));
    native(&mut row).memory.as_mut().unwrap().last_kb = Some(f64::MAX);
    let largest = explain(&[&row], None).unwrap();
    assert!(serde_json::to_string(&largest).is_ok(), "native KiB must not overflow through derived bytes or totals");
}

#[test]
fn native_incarnations_stay_distinct_and_duplicate_keys_or_mismatched_sources_are_rejected() {
    let row = observation("2");
    let mut changed = row.clone();
    native(&mut changed).identity.qualifiers.insert("context_settings_id".into(), "6".into());
    changed.key = native(&mut changed).key().unwrap();
    assert_eq!(explain(&[&row, &changed], None).unwrap().entries.len(), 2);
    assert!(explain(&[&row, &row], None).is_err());
    changed.source = "other native source".into();
    assert!(explain(&[&row, &changed], None).is_err());
    changed = row.clone();
    changed.key = digest(b"wrong native key");
    assert!(explain(&[&changed], None).is_err());
    changed = row.clone();
    native(&mut changed).average_duration_us = f64::NAN;
    assert!(explain(&[&changed], None).is_err());
}

#[test]
fn empty_unavailable_partial_and_unknown_sources_never_become_healthy_compilation_history() {
    let row = observation("2");
    for state in [SourceState::Empty, SourceState::Unavailable, SourceState::NotApplicable] {
        let source = SourceOutcome {
            source: row.source.clone(),
            state,
            reason: Some("qualification".into()),
            reason_message: None,
        };
        let report = explain(&[], Some(&source)).unwrap();
        assert_eq!(report.source_state, Some(state));
        assert_eq!(report.source_reason, source.reason);
        assert!(report.entries.is_empty());
        assert!(explain(&[&row], Some(&source)).is_err());
    }
    let source =
        SourceOutcome { source: row.source.clone(), state: SourceState::Partial, reason: None, reason_message: None };
    let partial = explain(&[&row], Some(&source)).unwrap();
    assert_eq!(partial.source_state, Some(SourceState::Partial));
    assert!(partial.limitations.contains(&t!("view.compilation.limit.source_partial")));
    assert!(explain(&[], Some(&source)).is_ok());
    let collected = SourceOutcome { state: SourceState::Collected, ..source };
    assert!(explain(&[], Some(&collected)).is_err());
    let unknown = explain(&[&row], None).unwrap();
    assert!(unknown.limitations.contains(&t!("view.compilation.limit.source_unknown")));
}
