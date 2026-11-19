//! Reuse the stored native-shaped corpus and canonical capture owner; no native cache mutation is claimed.
use super::*;

#[tokio::test]
async fn original_selected_populations_explain_additional_set_context_without_creation_or_execution_claims() {
    let [(before_source, before_rows), (after_source, after_rows)] =
        test_support::cache_entry_memory_comparison_evidence();
    let before = Fixture::with_sequence(before_source.clone(), before_rows.clone(), 100_201).await;
    let after = Fixture::with_sequence(after_source.clone(), after_rows.clone(), 100_202).await;
    let recovery_reads = (before.recovery_object_reads(), after.recovery_object_reads());
    let previous = before.read_families(FAMILY).await;
    let current = after.read_families(FAMILY).await;
    assert_ne!(previous.range_id(), current.range_id(), "the comparison needs two independent original captures");
    assert!(previous.is_read_complete() && current.is_read_complete());
    let analysis = collected_resources::cache_entry_memory(&current, Some(&previous)).unwrap();
    assert_eq!(analysis.context.source, after_source);
    let baseline = analysis.baseline_context.as_ref().unwrap();
    assert_eq!(baseline.source, before_source);
    let comparison = analysis.report.comparison.as_ref().unwrap();
    assert_eq!((comparison.entries_before, comparison.entries_after), (1, 2));
    assert_eq!(comparison.bytes_before, "9007199254740993");
    assert_eq!(comparison.bytes_after, "9007199254872065");
    assert_eq!(comparison.retained_entries.len(), 1);
    assert_eq!(comparison.retained_entries[0].metrics[1].text.as_deref(), Some("2"));
    assert_eq!(comparison.additional_contexts.len(), 1);
    assert!(comparison.additional_contexts[0].explanation.contains("ARITH_ABORT"));
    for (context, capture, rows) in [(&analysis.context, &current, &after_rows), (baseline, &previous, &before_rows)] {
        for row in rows {
            let original = ExportedRecord::new(&context.source, capture.capture(), row.clone()).unwrap();
            assert!(context.records.iter().any(|reference| reference.row == original));
        }
    }
    let report = serde_json::to_string(&analysis.report).unwrap();
    for private in ["SELECT value", "9264", "fix_sql", "causal_share", "severity"] {
        assert!(!report.contains(private), "{private}");
    }
    assert_eq!((before.recovery_object_reads(), after.recovery_object_reads()), recovery_reads);
}

#[tokio::test]
async fn missing_or_unpublished_baseline_body_and_foreign_native_process_never_lend_a_selected_population() {
    for (index, gap) in ["missing", "unpublished", "process", "role"].into_iter().enumerate() {
        let [(mut source, rows), (after_source, after_rows)] = test_support::cache_entry_memory_comparison_evidence();
        if gap == "process" {
            let SourcePartition::InstanceLocal { epoch, .. } = &mut source.partition else { panic!() };
            *epoch = Some(digest(b"another-native-process"));
            let SourceObservation::CacheEntryMemory { focus, .. } = &mut source.observation else { panic!() };
            focus.context.partition = source.partition.clone();
        } else if gap == "role" {
            let SourceObservation::CacheEntryMemory { focus, .. } = &mut source.observation else { panic!() };
            focus.context.database_writable = None;
        }
        let sequence = 100_210 + index as u64 * 2;
        let before = Fixture::with_sequence(source, rows, sequence).await;
        if gap == "missing" {
            before.remove_first_row(TelemetryStream::OperationalSamples);
        } else if gap == "unpublished" {
            before.unpublish();
        }
        let after = Fixture::with_sequence(after_source, after_rows, sequence + 1).await;
        let recovery_reads = (before.recovery_object_reads(), after.recovery_object_reads());
        let previous = before.read_families(FAMILY).await;
        let current = after.read_families(FAMILY).await;
        let analysis = collected_resources::cache_entry_memory(&current, Some(&previous)).unwrap();
        assert!(analysis.report.comparison.is_none(), "{gap}");
        assert_eq!(analysis.report.entries.len(), 2, "{gap}: retain current positive entry facts");
        assert!(analysis.baseline_context.is_some());
        assert_eq!(
            (before.recovery_object_reads(), after.recovery_object_reads()),
            recovery_reads,
            "{gap}: no implicit recovery"
        );
    }
}

#[tokio::test]
async fn a_baseline_is_explicit_distinct_and_of_the_original_cache_entry_dataset() {
    let [_, (source, rows)] = test_support::cache_entry_memory_comparison_evidence();
    let fixture = Fixture::new(source, rows).await;
    let current = fixture.read_families(FAMILY).await;
    let ordinary = collected_resources::cache_entry_memory(&current, None).unwrap();
    assert!(ordinary.report.comparison.is_none() && ordinary.baseline_context.is_none());
    assert_eq!(
        collected_resources::cache_entry_memory(&current, Some(&current)).unwrap_err().detail,
        Some(ErrorDetail::Delivery(DeliveryProblem::InvalidContract))
    );
    let (source, rows) = test_support::statement_grant_evidence(test_support::StatementGrantCase::Complete);
    let unrelated = Fixture::with_sequence(source, rows, 100_230).await;
    let baseline = unrelated.read_families(FAMILY).await;
    assert_eq!(
        collected_resources::cache_entry_memory(&current, Some(&baseline)).unwrap_err().detail,
        Some(ErrorDetail::Delivery(DeliveryProblem::UnsupportedSchema))
    );
}
