use super::*;
use culprit_collector::analytical::test_support::compilation_interval_evidence;
use culprit_core::CompilationComparisonIssue;

#[tokio::test]
async fn authenticated_reads_keep_both_original_proofs_and_compute_exact_native_count_changes() {
    let [(before_source, before_rows), (after_source, after_rows)] = compilation_interval_evidence();
    let before = Fixture::with_sequence(before_source.clone(), before_rows.clone(), 101001).await;
    let after = Fixture::with_sequence(after_source.clone(), after_rows.clone(), 101002).await;
    let a = before.read_families(&[EvidenceFamily::OperationalSamples]).await;
    let b = after.read_families(&[EvidenceFamily::OperationalSamples]).await;
    let analysis = collected_resources::compilation(&b, Some(&a), None).unwrap();
    assert_eq!(analysis.context.source, after_source);
    let original = analysis.baseline_context.as_ref().unwrap();
    assert_eq!(original.source, before_source);
    assert_eq!(original.records[0].row.record, before_rows[0]);
    assert_eq!(analysis.context.records[0].row.record, after_rows[0]);
    assert_ne!(analysis.context.range_id, original.range_id);
    let comparison = analysis.report.comparison.as_ref().unwrap();
    assert!(comparison.issues.is_empty());
    assert_eq!(comparison.entries[0].compilations, "1");
    assert!(comparison.baseline.is_none(), "an archive reference never invents a scan id");
    assert_eq!(a.metrics().recovery_objects + b.metrics().recovery_objects, 0);
    let same = collected_resources::compilation(&a, Some(&a), None).unwrap_err();
    assert_eq!(same.detail, Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::InvalidContract)));
    let json = serde_json::to_string(&analysis).unwrap();
    assert_eq!(
        json.matches("\"kind\":\"mssql-query-compilation\"").count(),
        2,
        "raw facts occur exactly once per original read"
    );
    assert!(!json.contains("duration_delta") && !json.contains("memory_total"));
}

#[tokio::test]
async fn partial_archive_reads_preserve_counter_evidence_and_withhold_missing_rows_without_zero_filling() {
    let [(before_source, before_rows), (mut after_source, after_rows)] = compilation_interval_evidence();
    let before = Fixture::with_sequence(before_source, before_rows, 101011).await;
    let SourceObservation::Snapshot(snapshot) = &mut after_source.observation else { panic!() };
    snapshot.sources[0].state = SourceState::Partial;
    snapshot.sources[0].complete_scope = false;
    let after = Fixture::with_sequence(after_source, after_rows, 101012).await;
    let a = before.read_families(&[EvidenceFamily::OperationalSamples]).await;
    let b = after.read_families(&[EvidenceFamily::OperationalSamples]).await;
    let analysis = collected_resources::compilation(&b, Some(&a), None).unwrap();
    let comparison = analysis.report.comparison.as_ref().unwrap();
    assert_eq!(comparison.entries[0].compilations, "1");
    assert!(comparison.issues.contains(&CompilationComparisonIssue::PartialRead));
    assert_eq!(analysis.context.source, b.source().clone());
    after.remove_first_row(TelemetryStream::OperationalSamples);
    let missing = after.read_families(&[EvidenceFamily::OperationalSamples]).await;
    assert!(!missing.is_read_complete());
    let analysis = collected_resources::compilation(&missing, Some(&a), None).unwrap();
    let comparison = analysis.report.comparison.unwrap();
    assert!(comparison.entries.is_empty());
    assert_eq!(comparison.missing_after, 1);
    assert!(comparison.issues.contains(&CompilationComparisonIssue::UnmatchedEntries));
}

#[tokio::test]
async fn another_database_incarnation_cannot_supply_a_compilation_baseline_even_at_the_same_endpoint() {
    let [(mut before_source, before_rows), (after_source, after_rows)] = compilation_interval_evidence();
    let CollectionTarget::Database(database) = &mut before_source.target else { panic!() };
    database.incarnation = Some("different-original-native-database".into());
    let before = Fixture::with_sequence(before_source, before_rows, 101021).await;
    let after = Fixture::with_sequence(after_source, after_rows, 101022).await;
    let a = before.read_families(&[EvidenceFamily::OperationalSamples]).await;
    let b = after.read_families(&[EvidenceFamily::OperationalSamples]).await;
    let analysis = collected_resources::compilation(&b, Some(&a), None).unwrap();
    let comparison = analysis.report.comparison.unwrap();
    assert_eq!(comparison.issues, [CompilationComparisonIssue::DifferentSource]);
    assert!(comparison.entries.is_empty() && comparison.observed_from.is_none());
    assert_eq!(analysis.context.records.len(), 1);
    assert_eq!(analysis.baseline_context.unwrap().records.len(), 1);
}
