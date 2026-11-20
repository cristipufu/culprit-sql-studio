use super::*;
use culprit_checks::performance_counters::PerformanceCounterEvidence;
use culprit_collector::analytical::test_support::{
    DatabasePerformanceCounterCase, PerformanceCounterCase, database_performance_counter_evidence,
    performance_counter_evidence,
};
use culprit_core::telemetry::*;

fn report(case: PerformanceCounterCase, missing: bool) -> PerformanceCounterReport {
    let (source, records) = performance_counter_evidence(case);
    let context = NativeReadContext::from_snapshot(&source).unwrap();
    let coverage = &source.observation.snapshot().unwrap().sources[0];
    let rows = records
        .iter()
        .filter_map(|record| {
            let TelemetryRecord::Operational(row) = record else { panic!() };
            (!missing && row.source == culprit_core::source_id::PERFORMANCE_COUNTERS).then_some(row)
        })
        .collect::<Vec<_>>();
    culprit_checks::performance_counters::explain(PerformanceCounterEvidence::Captured {
        context: &context,
        coverage,
        observations: &rows,
        read_complete: !missing,
    })
    .unwrap()
}

fn database_report(case: DatabasePerformanceCounterCase) -> PerformanceCounterReport {
    let (source, records) = database_performance_counter_evidence(case);
    let coverage = &source.observation.snapshot().unwrap().sources[0];
    let observations = records
        .iter()
        .map(|row| {
            let TelemetryRecord::Operational(row) = row else { panic!() };
            row
        })
        .collect::<Vec<_>>();
    culprit_checks::performance_counters::explain_source(&source, coverage, &observations, true).unwrap()
}

#[test]
fn human_counter_output_keeps_exact_native_units_original_counts_clocks_and_every_qualification() {
    let report = report(PerformanceCounterCase::Exact, false);
    let text = performance_counters(&report);
    assert!(text.contains("9007199254740993 KiB"));
    let byte = report.entries.iter().position(|entry| entry.field == "log_bytes_flushed").unwrap();
    assert!(byte >= super::super::super::BRIEF_FINDING_LIMIT);
    assert!(!text.contains("9007199254740993 B"), "the bounded prefix must not print later native detail");
    assert!(text.contains("2020-01-01T12:00:00.500000000Z"));
    assert!(text.contains("2020-01-01T12:00:00.000000000Z"));
    for entry in report.entries.iter().take(super::super::super::BRIEF_FINDING_LIMIT) {
        assert!(text.contains(&entry.metric.label));
        assert!(text.contains(entry.metric.text.as_deref().unwrap()));
    }
    for limitation in &report.limitations {
        assert!(text.contains(limitation));
    }
    assert!(text.contains(&t!(
        "view.performance_counters.cli.printed",
        printed = format::digits(super::super::super::BRIEF_FINDING_LIMIT.to_string())
    )));
    assert_eq!(report.native_counter_rows, Some(11));
}

#[test]
fn missing_counter_body_and_denied_source_do_not_render_zero_or_native_field_names() {
    for report in [report(PerformanceCounterCase::Exact, true), report(PerformanceCounterCase::Denied, false)] {
        let text = performance_counters(&report);
        assert!(text.contains(&report.summary));
        assert!(!text.contains("9007199254740993"));
        assert!(report.entries.is_empty());
        assert!(!text.contains("Granted Workspace Memory (KB)"));
        for limitation in &report.limitations {
            assert!(text.contains(limitation));
        }
    }
}

#[test]
fn counter_json_keeps_raw_exactness_unknown_type_and_partial_native_visibility() {
    let unknown = report(PerformanceCounterCase::UnknownNativeType, false);
    let json = serde_json::to_value(&unknown).unwrap();
    assert_eq!(json["entries"][0]["raw_value"], "9007199254740993");
    assert_eq!(json["entries"][0]["availability"], "unknown-native-type");
    assert!(performance_counters(&unknown).contains(&t!("view.performance_counters.limit.native_type")));
    let partial = report(PerformanceCounterCase::NativePartial, false);
    assert!(performance_counters(&partial).contains(&t!("evidence.cli.source_partial")));
}

#[test]
fn a_byte_counter_in_the_printed_prefix_keeps_exact_units_without_changing_source_or_printed_counts() {
    let mut report = report(PerformanceCounterCase::Exact, false);
    let byte = report.entries.iter().position(|entry| entry.field == "log_bytes_flushed").unwrap();
    // Presentation order grants no new evidence: the same interpreted entry can occur in any native row order.
    report.entries.swap(0, byte);
    let text = performance_counters(&report);
    assert!(text.contains("9007199254740993 B"));
    assert!(!text.contains("9007199254740992 B"));
    assert_eq!(report.native_counter_rows, Some(11));
    assert_eq!(report.observed_counter_rows, 11);
    assert_eq!(report.presented_counter_rows, 11);
    assert!(text.contains(&t!(
        "view.performance_counters.cli.printed",
        printed = format::digits(super::super::super::BRIEF_FINDING_LIMIT.to_string())
    )));
    let first_omitted = &report.entries[super::super::super::BRIEF_FINDING_LIMIT];
    assert!(!text.contains(&first_omitted.native_name));
}

#[test]
fn selected_database_counter_text_preserves_original_database_fields_exact_values_and_all_qualifications() {
    use DatabasePerformanceCounterCase as Case;
    for case in Case::ALL {
        let report = database_report(case);
        let text = performance_counters(&report);
        assert!(text.contains(&t!("view.performance_counters.database", database = "shop", id = "7")));
        for limitation in &report.limitations {
            assert!(text.contains(limitation));
        }
        assert!(report.ratios.is_none());
        assert_eq!(report.native_database_id.as_deref(), Some("7"));
        if case == Case::Exact {
            assert!(text.contains("9007199254740993 B"));
            assert_eq!(report.entries.len(), 3);
        } else if case == Case::Denied {
            assert!(report.entries.is_empty());
            assert_eq!(report.native_counter_rows, None);
        }
        for private in [
            "private-database-guid-canary",
            "private-fork-guid-canary",
            "private-file-guid-canary",
            "private-process-canary",
        ] {
            assert!(!text.contains(private));
        }
    }
}
#[test]
fn database_and_native_entry_labels_use_bounded_escaped_previews_without_mutating_the_raw_report() {
    let mut report = database_report(DatabasePerformanceCounterCase::Exact);
    let name = format!("native\n\u{1b}数据库{}", "界".repeat(500));
    report.database = Some(name.clone());
    for entry in &mut report.entries {
        entry.native_instance = Some(name.clone());
    }
    let object = "SQLServer\n\u{1b}:Databases";
    let counter = "Log\n\u{1b}Bytes Flushed/sec";
    report.entries[0].native_object = object.into();
    report.entries[0].native_name = counter.into();
    let original = serde_json::to_value(&report).unwrap();
    let text = performance_counters(&report);
    for label in [&name, object, counter] {
        assert!(text.contains(&preview(label)));
    }
    assert!(!text.contains('\u{1b}'));
    assert!(!text.contains("native\n"));
    assert!(!text.contains("SQLServer\n"));
    assert!(!text.contains("Log\n"));
    assert!(!text.contains(&"界".repeat(101)));
    assert_eq!(serde_json::to_value(&report).unwrap(), original);
}

#[test]
fn saved_report_populations_keep_each_native_scope_counts_bounds_and_detail_limit() {
    let shared = report(PerformanceCounterCase::Exact, false);
    let selected = database_report(DatabasePerformanceCounterCase::Partial);
    let expected_shared = performance_counters(&shared);
    let expected_selected = performance_counters(&selected);
    let text = populations(&[shared, selected]);
    assert_eq!(text, format!("{expected_shared}\n\n{expected_selected}"));
    assert!(text.contains("9007199254740993 KiB"));
    assert!(text.contains("9007199254740993 B"));
    assert!(text.contains(&t!("view.performance_counters.database", database = "shop", id = "7")));
    assert!(text.contains(&t!("evidence.cli.source_partial")));
}

#[test]
fn withdrawn_selected_population_has_explicit_scope_and_unknown_original_counts() {
    let reason = culprit_core::message!("view.performance_counters.sharing_withheld");
    let selected =
        culprit_checks::performance_counters::unavailable(PerformanceCounterPopulation::SelectedDatabase, &reason);
    let text = populations(&[selected]);
    assert!(text.contains(&t!("view.performance_counters.selected_scope")));
    assert!(text.contains(&reason.render()));
    assert!(text.contains(&t!("view.performance_counters.limit.historical")));
    assert!(!text.contains(&t!("view.performance_counters.scope")));
    assert!(!text.contains("SQLServer:Databases"));
    assert!(!text.contains("9007199254740993"));
}
