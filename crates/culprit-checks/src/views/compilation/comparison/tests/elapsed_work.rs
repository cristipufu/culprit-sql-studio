use super::*;

fn endpoints() -> (CompilationBaseline, OperationalObservation, NativeReadContext) {
    let mut old = observation("10");
    native(&mut old).average_duration_us = 1_000_000.0;
    let previous = baseline(&old);
    let mut row = current("30");
    native(&mut row).average_duration_us = 2_000_000.0;
    native(&mut row).last_compile_at = "2020-01-01T13:00:01Z".into();
    native(&mut row).seconds_since_first_compile = 25201;
    (previous, row, context("2020-01-01T13:00:01Z", "2020-01-01T13:00:02Z"))
}

fn work(
    previous: &CompilationBaseline,
    row: &mut OperationalObservation,
    after: &NativeReadContext,
) -> Option<super::super::elapsed_work::CompileWork> {
    let before = &previous.points[0];
    let count = native(row).compilations.parse::<u64>().unwrap();
    let delta = count.checked_sub(before.compilations.parse::<u64>().unwrap())?;
    super::super::elapsed_work::from_endpoints(previous.context.as_ref().unwrap(), after, before, native(row), delta)
}

#[test]
fn matched_native_cumulative_means_estimate_interval_elapsed_work_and_reuse_material_cost_policy() {
    let (previous, mut row, after) = endpoints();
    let actual = work(&previous, &mut row, &after).unwrap();
    assert_eq!(actual.milliseconds(), 50_000.0, "30 × 2 s − 10 × 1 s is 50 s; current mean × delta would give 40 s");
    assert!(actual.is_material());
    assert_eq!(actual.hourly_rate(), Some(20.0 / 3602.0 * 3600.0));
    let report = compare(Some(&after), &[&row], Some(&source(SourceState::Collected)), &previous).unwrap();
    let entry = &report.entries[0];
    assert_eq!(entry.compilations, "20");
    assert_eq!(entry.identity, previous.points[0].identity);
    assert_eq!(entry.investigation, t!("view.compilation.comparison.investigate.cadence"));
    assert!(
        entry
            .explanation
            .contains(&t!("view.compilation.comparison.entry.elapsed", elapsed = format::duration(50_000.0)))
    );
    assert!(report.limitations.contains(&t!("view.compilation.comparison.limit.estimated")));
}

#[test]
fn rates_require_a_full_native_hour_even_when_float_epoch_rounding_would_erase_one_nanosecond() {
    let (previous, mut row, _) = endpoints();
    for (from, until, rate) in [
        ("2020-01-01T12:59:59Z", "2020-01-01T13:00:02Z", false),
        ("2020-01-01T13:00:00.999999999Z", "2020-01-01T13:00:02Z", false),
        ("2020-01-01T13:00:01Z", "2020-01-01T13:00:02Z", true),
    ] {
        let after = context(from, until);
        let actual = work(&previous, &mut row, &after).unwrap();
        assert_eq!(actual.hourly_rate().is_some(), rate, "{from}");
        assert_eq!(actual.milliseconds(), 50_000.0);
    }
}

#[test]
fn wide_counts_products_and_cancellation_uncertainty_keep_exact_changes_without_zero_work() {
    for (before, after, mean) in [
        ("9007199254740993", "9007199254740994", 1.0),
        ("1", "2", 9_007_199_254_740_992.0),
        ("4503599627370496", "4503599627370497", 1.0),
        ("0", "1", 1_000_000.0),
    ] {
        let (mut previous, mut row, after_context) = endpoints();
        previous.points[0].compilations = before.into();
        previous.points[0].average_duration_us = mean;
        native(&mut row).compilations = after.into();
        native(&mut row).average_duration_us = mean;
        assert!(work(&previous, &mut row, &after_context).is_none(), "{before} {after}");
        let report = compare(Some(&after_context), &[&row], Some(&source(SourceState::Collected)), &previous).unwrap();
        assert_eq!(report.entries[0].compilations, "1");
        assert!(report.entries[0].explanation.ends_with(&t!("view.compilation.comparison.entry.elapsed_unavailable")));
    }
}

#[test]
fn regressing_zero_or_clock_unqualified_aggregates_withhold_cost_without_losing_counter_evidence() {
    for case in ["decreasing", "zero", "sub-microsecond", "negative-native-clock", "old-compile-clock"] {
        let (mut previous, mut row, after) = endpoints();
        match case {
            "decreasing" => native(&mut row).average_duration_us = 1.0,
            "zero" => {
                previous.points[0].average_duration_us = 0.0;
                native(&mut row).average_duration_us = 0.0;
            }
            "sub-microsecond" => {
                previous.points[0].average_duration_us = 0.001;
                native(&mut row).average_duration_us = 0.001;
            }
            "negative-native-clock" => native(&mut row).seconds_since_first_compile = -1,
            _ => native(&mut row).last_compile_at = "2020-01-01T12:00:00.9Z".into(),
        }
        assert!(work(&previous, &mut row, &after).is_none(), "{case}");
        let report = compare(Some(&after), &[&row], Some(&source(SourceState::Collected)), &previous).unwrap();
        assert_eq!(report.entries[0].compilations, "20");
        assert_eq!(report.entries[0].investigation, t!("view.compilation.comparison.investigate"));
    }
}

#[test]
fn last_maximum_memory_and_stage_cpu_cannot_change_elapsed_work_or_supply_concurrent_demand() {
    let (previous, mut row, after) = endpoints();
    let expected = compare(Some(&after), &[&row], Some(&source(SourceState::Collected)), &previous).unwrap();
    let native = native(&mut row);
    native.memory = Some(culprit_core::CompilationMemory {
        average_kb: Some(1000.0),
        last_kb: Some(9007199254740992.0),
        maximum_kb: Some(9007199254740992.0),
    });
    native.timings.optimization.average_cpu_us = Some(9007199254740992.0);
    let actual = compare(Some(&after), &[&row], Some(&source(SourceState::Collected)), &previous).unwrap();
    assert_eq!(serde_json::to_value(actual).unwrap(), serde_json::to_value(expected).unwrap());
}

#[test]
fn cheap_or_unchanged_compilation_history_never_acquires_material_cadence_advice() {
    let (mut previous, mut row, after) = endpoints();
    previous.points[0].average_duration_us = 1.0;
    native(&mut row).average_duration_us = 1.0;
    let actual = work(&previous, &mut row, &after).unwrap();
    assert_eq!(actual.milliseconds(), 0.02);
    assert!(!actual.is_material());
    let report = compare(Some(&after), &[&row], Some(&source(SourceState::Collected)), &previous).unwrap();
    assert_eq!(report.entries[0].investigation, t!("view.compilation.comparison.investigate"));
    native(&mut row).compilations = "10".into();
    native(&mut row).last_compile_at = previous.points[0].last_compile_at.clone();
    let report = compare(Some(&after), &[&row], Some(&source(SourceState::Collected)), &previous).unwrap();
    assert_eq!(report.entries[0].compilations, "0");
    assert_eq!(report.entries[0].explanation, t!("view.compilation.comparison.entry.unchanged"));
}

#[test]
fn partial_rows_keep_independent_cost_but_missing_sources_changed_native_context_and_resets_do_not() {
    let (previous, mut row, after) = endpoints();
    let partial = compare(Some(&after), &[&row], Some(&source(SourceState::Partial)), &previous).unwrap();
    assert_eq!(partial.entries[0].compilations, "20");
    assert!(partial.issues.contains(&Issue::PartialRead));
    for case in ["reset", "namespace", "context-settings", "missing-source"] {
        let mut previous = previous.clone();
        let mut after = after.clone();
        let mut current = row.clone();
        match case {
            "reset" => previous.points[0].reset = culprit_core::ResetMarker::Generation("changed".into()),
            "namespace" => {
                let CollectionTarget::Database(database) = &mut after.target else { panic!() };
                database.incarnation = Some("foreign".into());
            }
            "context-settings" => {
                native(&mut current).identity.qualifiers.insert("context_settings_id".into(), "6".into());
                current.key = native(&mut current).key().unwrap();
            }
            _ => {}
        }
        let source = source(if case == "missing-source" { SourceState::Unavailable } else { SourceState::Collected });
        let rows = if case == "missing-source" { vec![] } else { vec![&current] };
        let actual = compare(Some(&after), &rows, Some(&source), &previous).unwrap();
        assert!(actual.entries.is_empty(), "{case}");
    }
    native(&mut row).last_compile_at = "2020-01-01T14:00:00Z".into();
    assert!(
        compare(Some(&after), &[&row], Some(&source(SourceState::Collected)), &previous).unwrap().entries.is_empty()
    );
}
