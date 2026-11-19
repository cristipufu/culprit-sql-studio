use super::*;
use culprit_collector::analytical::test_support::{CacheEntryMemoryCase, cache_entry_memory_evidence};
use culprit_core::telemetry::*;

fn report(case: CacheEntryMemoryCase) -> CacheEntryMemoryReport {
    let (source, records) = cache_entry_memory_evidence(case);
    explain(&source, &records)
}

fn explain(source: &CollectionDescription, records: &[TelemetryRecord]) -> CacheEntryMemoryReport {
    let context = NativeReadContext::from_snapshot(source).unwrap();
    let coverage = &source.observation.snapshot().unwrap().sources[0];
    let source =
        SourceOutcome { source: coverage.source.clone(), state: coverage.state, reason: None, reason_message: None };
    let rows = records
        .iter()
        .map(|record| match record {
            TelemetryRecord::Operational(row) => row,
            _ => panic!("cache-entry corpus"),
        })
        .collect::<Vec<_>>();
    culprit_checks::cache_entry_memory::explain(&context, &rows, Some(&source)).unwrap()
}

fn assert_no_original_sql(text: &str) {
    for native in
        ["SELECT value", "dbo.cache_input", "9264", "source_text_bytes", "statement_start_bytes", "sql_handle"]
    {
        assert!(!text.contains(native), "{native}");
    }
}

fn population_report() -> CacheEntryMemoryReport {
    let [(before_source, before_records), (after_source, after_records)] =
        culprit_collector::analytical::test_support::cache_entry_memory_comparison_evidence();
    let evidence = |source: &CollectionDescription| {
        let mut context = NativeReadContext::from_snapshot(source).unwrap();
        let SourceObservation::CacheEntryMemory { focus, snapshot } = &source.observation else { panic!() };
        context.database_writable = focus.context.database_writable;
        let source = SourceOutcome {
            source: snapshot.sources[0].source.clone(),
            state: snapshot.sources[0].state,
            reason: None,
            reason_message: None,
        };
        (context, source)
    };
    fn rows(records: &[TelemetryRecord]) -> Vec<&OperationalObservation> {
        records
            .iter()
            .map(|record| {
                let TelemetryRecord::Operational(row) = record else { panic!() };
                row
            })
            .collect()
    }
    let (before_context, before_coverage) = evidence(&before_source);
    let (after_context, after_coverage) = evidence(&after_source);
    let before_rows = rows(&before_records);
    let after_rows = rows(&after_records);
    culprit_checks::cache_entry_memory::compare(
        culprit_checks::cache_entry_memory::CacheEntryMemoryEvidence {
            context: &before_context,
            observations: &before_rows,
            source: Some(&before_coverage),
            read_complete: true,
        },
        culprit_checks::cache_entry_memory::CacheEntryMemoryEvidence {
            context: &after_context,
            observations: &after_rows,
            source: Some(&after_coverage),
            read_complete: true,
        },
    )
    .unwrap()
}

#[test]
fn selected_population_comparison_keeps_shared_exact_totals_named_context_and_lookup_meaning_without_sql() {
    let report = population_report();
    let comparison = report.comparison.as_ref().unwrap();
    let text = cache_entry_memory(&report);
    assert!(text.contains(&comparison.title) && text.contains(&comparison.summary));
    assert!(text.contains("9007199254740993") && text.contains("9007199254872065"));
    assert!(!text.contains("9007199254740992"));
    for limit in &comparison.limitations {
        assert!(text.contains(limit));
    }
    let retained = &comparison.retained_entries[0];
    assert!(text.contains(&retained.explanation) && text.contains(&retained.verify));
    assert!(text.contains(&comparison.additional_contexts[0].explanation));
    assert!(text.contains("ARITH_ABORT"));
    assert_no_original_sql(&text);
}

#[test]
fn population_comparison_refuses_missing_or_ambiguous_links_and_keeps_independent_fields_bounded() {
    let original = population_report();
    for broken in ["missing-current", "foreign-anchor", "duplicate-anchor"] {
        let mut report = original.clone();
        let comparison = report.comparison.as_mut().unwrap();
        comparison.additional_contexts[0].explanation = "additional-context-link-canary".into();
        match broken {
            "missing-current" => comparison.additional_contexts[0].right_observation_key = "foreign-current".into(),
            "foreign-anchor" => comparison.additional_contexts[0].left_observation_key = "foreign-anchor".into(),
            "duplicate-anchor" => comparison.retained_entries.push(comparison.retained_entries[0].clone()),
            _ => unreachable!(),
        }
        let summary = comparison.summary.clone();
        let text = cache_entry_memory(&report);
        assert!(!text.contains("additional-context-link-canary"), "{broken}");
        assert!(text.contains(&summary));
    }
    let mut report = original;
    let comparison = report.comparison.as_mut().unwrap();
    comparison.summary = format!("{}private-population-tail", "列".repeat(OBSERVATION_DETAIL_CHARS * 2));
    comparison.retained_entries[0].explanation =
        format!("{}private-retained-tail", "列".repeat(OBSERVATION_DETAIL_CHARS * 2));
    comparison.retained_entries[0].verify = "retained-verification-canary".into();
    let text = cache_entry_memory(&report);
    assert!(!text.contains("private-population-tail") && !text.contains("private-retained-tail"));
    assert!(text.contains("retained-verification-canary"));
    assert!(text.len() < 64 * 1024);
}

#[test]
fn named_set_options_keep_exact_whole_entry_metrics_shared_mechanism_and_investigation() {
    let report = report(CacheEntryMemoryCase::SetOptions);
    let text = cache_entry_memory(&report);
    assert_eq!(report.entries.len(), 2);
    assert_eq!(report.context_differences.len(), 1);
    for entry in &report.entries {
        for field in [&entry.title, &entry.explanation, &entry.investigation, &entry.verify] {
            assert!(text.contains(field), "{field}");
        }
        for metric in &entry.metrics {
            assert!(text.contains(&format!("{}: {}", metric.label, metric.text.as_ref().unwrap())));
        }
    }
    assert!(text.contains("9007199254740993") && text.contains("131072"));
    assert!(!text.contains("9007199254740992"));
    let difference = &report.context_differences[0];
    assert!(difference.explanation.contains("ARITH_ABORT"));
    assert!(text.contains(&difference.explanation) && text.contains(&difference.investigation));
    assert_no_original_sql(&text);
}

#[test]
fn original_row_order_does_not_change_the_shared_pair_or_its_native_setting_direction() {
    let (source, mut records) = cache_entry_memory_evidence(CacheEntryMemoryCase::SetOptions);
    let original = cache_entry_memory(&explain(&source, &records));
    records.reverse();
    assert_eq!(cache_entry_memory(&explain(&source, &records)), original);
}

#[test]
fn unproved_settings_keep_the_shared_generic_context_and_restricted_bodies_stay_private() {
    for case in [
        CacheEntryMemoryCase::UnsupportedSetOptions,
        CacheEntryMemoryCase::OtherContext,
        CacheEntryMemoryCase::MissingPairSql,
    ] {
        let report = report(case);
        let text = cache_entry_memory(&report);
        let difference = &report.context_differences[0];
        assert_eq!(
            difference.explanation,
            t!("view.cache_entry_memory.context.difference", attributes = difference.attributes.join(", "))
        );
        assert!(text.contains(&difference.explanation) && text.contains(&difference.investigation));
        assert!(!text.contains("ARITH_ABORT"), "{case:?}");
        assert_no_original_sql(&text);
    }
    for case in [CacheEntryMemoryCase::UnknownCacheKeys, CacheEntryMemoryCase::MissingSql] {
        let report = report(case);
        let text = cache_entry_memory(&report);
        assert!(report.context_differences.is_empty());
        assert!(text.contains(&report.entries[0].explanation));
        assert_no_original_sql(&text);
    }
}

#[test]
fn partial_empty_and_unavailable_sources_keep_every_original_qualification_without_inventing_entries() {
    for case in [
        CacheEntryMemoryCase::Partial,
        CacheEntryMemoryCase::Empty,
        CacheEntryMemoryCase::UnavailableSource,
        CacheEntryMemoryCase::UnknownCacheKeys,
    ] {
        let report = report(case);
        let text = cache_entry_memory(&report);
        for limitation in &report.limitations {
            assert!(text.contains(limitation), "{case:?}: {limitation}");
        }
        if report.source_state == Some(SourceState::Partial) {
            assert!(text.contains(&t!("evidence.cli.source_partial")));
        }
        if report.source_state == Some(SourceState::Unavailable) {
            assert!(text.contains(&t!("evidence.cli.source_unavailable")));
        }
        if report.entries.is_empty() {
            assert!(!text.contains("9007199254740993"));
        }
    }
}

#[test]
fn comparisons_require_two_unique_displayed_entry_keys_in_the_shared_order() {
    let original = report(CacheEntryMemoryCase::SetOptions);
    let difference = &original.context_differences[0];
    let left = original.entries.iter().find(|entry| entry.observation_key == difference.left_observation_key).unwrap();
    let right =
        original.entries.iter().find(|entry| entry.observation_key == difference.right_observation_key).unwrap();
    assert!(cache_entry_memory(&original).contains(&t!(
        "evidence.cli.cache_entry_memory.context_pair",
        left = &left.title,
        right = &right.title
    )));
    for invalid in ["missing", "foreign", "duplicate", "self", "hidden"] {
        let mut report = original.clone();
        match invalid {
            "missing" => report.entries.retain(|entry| entry.observation_key != difference.right_observation_key),
            "foreign" => report.context_differences[0].right_observation_key = "foreign-entry".into(),
            "duplicate" => report.entries.push(left.clone()),
            "self" => report.context_differences[0].right_observation_key = difference.left_observation_key.clone(),
            "hidden" => {
                let right = report.entries.remove(
                    report
                        .entries
                        .iter()
                        .position(|entry| entry.observation_key == difference.right_observation_key)
                        .unwrap(),
                );
                for index in 0..super::super::super::BRIEF_FINDING_LIMIT {
                    let mut unrelated = left.clone();
                    unrelated.observation_key = format!("unrelated-{index}");
                    report.entries.push(unrelated);
                }
                report.entries.push(right);
            }
            _ => unreachable!(),
        }
        let text = cache_entry_memory(&report);
        assert!(!text.contains(&difference.explanation), "{invalid}");
        assert!(!text.contains(&difference.investigation), "{invalid}");
        assert!(text.contains(&left.explanation));
    }
}

#[test]
fn long_fields_preserve_later_metrics_verification_and_comparisons_with_finite_output() {
    let mut report = report(CacheEntryMemoryCase::SetOptions);
    let long = |label: &str| format!("{label}{}private-tail-{label}", "列".repeat(OBSERVATION_DETAIL_CHARS * 2));
    report.title = long("report-title");
    report.summary = long("report-summary");
    report.source_reason = Some(long("source-reason"));
    report.limitations = (0..20).map(|index| long(&format!("limit-{index}:"))).collect();
    let template = report.entries[0].clone();
    for index in report.entries.len()..8 {
        let mut entry = template.clone();
        entry.observation_key = format!("additional-entry-{index}");
        report.entries.push(entry);
    }
    for (index, entry) in report.entries.iter_mut().enumerate() {
        entry.title = format!("entry-{index}:{}private-title-tail-{index}", "列".repeat(OBSERVATION_DETAIL_CHARS * 2));
        entry.explanation = long(&format!("mechanism-{index}:"));
        entry.metrics[0].text = Some(long(&format!("bytes-{index}:")));
        entry.investigation = format!("investigation-{index}");
        entry.verify = format!("verification-{index}");
        entry.metrics.extend(std::iter::repeat_n(entry.metrics[0].clone(), 100));
    }
    let difference = report.context_differences[0].clone();
    report.context_differences = (0..20)
        .map(|index| {
            let mut difference = difference.clone();
            difference.explanation = long(&format!("comparison-{index}:"));
            difference.investigation = format!("pair-investigation-{index}");
            difference
        })
        .collect();
    let text = cache_entry_memory(&report);
    for index in 0..super::super::super::BRIEF_FINDING_LIMIT {
        for field in
            ["entry", "mechanism", "bytes", "investigation", "verification", "comparison", "pair-investigation"]
        {
            assert!(text.contains(&format!("{field}-{index}")), "{field}-{index}");
        }
    }
    for tail in ["private-tail", "private-title-tail", "entry-3:", "comparison-3:", "limit-8:"] {
        assert!(!text.contains(tail), "{tail}");
    }
    assert_eq!(text.matches("bytes-0:").count(), 1);
    assert!(text.chars().count() < 40 * OBSERVATION_DETAIL_CHARS);
    assert_no_original_sql(&text);
}
