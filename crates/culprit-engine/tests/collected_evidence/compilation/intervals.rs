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
    assert!(
        comparison.entries[0]
            .explanation
            .ends_with(&culprit_core::t!("view.compilation.comparison.entry.elapsed_unavailable"))
    );
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
async fn matched_original_reads_replay_approximate_elapsed_work_without_borrowing_sql_or_compiler_memory() {
    for (case, available) in [(101031, true), (101041, false)] {
        let [(before_source, mut before_rows), (mut after_source, mut after_rows)] = compilation_interval_evidence();
        native(&mut before_rows[0]).compilations = "10".into();
        native(&mut before_rows[0]).average_duration_us = 1_000_000.0;
        let current = native(&mut after_rows[0]);
        current.compilations = "30".into();
        current.average_duration_us = 2_000_000.0;
        current.last_compile_at = "2020-01-01T13:00:01Z".into();
        current.seconds_since_first_compile = 25201;
        current.origin = StatementOrigin::Application;
        current.statement = if available {
            DefinitionPayload::sql("SELECT amount FROM dbo.orders WHERE id=9264", Engine::Mssql)
        } else {
            DefinitionPayload::Unavailable { reason: PayloadUnavailable::Restricted }
        };
        let expected_statement = current.statement.clone();
        let expected_memory = current.memory.clone();
        let expected_identity = current.identity.clone();
        let SourceObservation::Snapshot(point) = &mut after_source.observation else { panic!() };
        point.observed_from = "2020-01-01T13:00:01Z".into();
        point.observed_until = "2020-01-01T13:00:02Z".into();
        let before = Fixture::with_sequence(before_source.clone(), before_rows.clone(), case).await;
        let after = Fixture::with_sequence(after_source.clone(), after_rows.clone(), case + 1).await;
        let a = before.read_families(&[EvidenceFamily::OperationalSamples]).await;
        let b = after.read_families(&[EvidenceFamily::OperationalSamples]).await;
        assert!(a.is_read_complete() && b.is_read_complete());
        let analysis = collected_resources::compilation(&b, Some(&a), None).unwrap();
        assert_eq!(analysis.context.source, after_source);
        assert_eq!(analysis.baseline_context.as_ref().unwrap().source, before_source);
        assert_eq!(analysis.context.records[0].row.record, after_rows[0]);
        assert_eq!(analysis.baseline_context.as_ref().unwrap().records[0].row.record, before_rows[0]);
        let comparison = analysis.report.comparison.as_ref().unwrap();
        assert!(comparison.issues.is_empty());
        let changed = &comparison.entries[0];
        assert_eq!(changed.compilations, "20");
        assert_eq!(changed.identity, expected_identity);
        assert_eq!(changed.observation_key, changed.baseline_observation_key);
        assert_eq!(
            changed.explanation,
            [
                culprit_core::t!("view.compilation.comparison.entry.changed", count = "20"),
                culprit_core::t!(
                    "view.compilation.comparison.entry.elapsed",
                    elapsed = culprit_core::format::duration(50_000.0)
                ),
                culprit_core::t!(
                    "view.compilation.comparison.entry.cadence",
                    span = culprit_core::format::duration(3_602_000.0),
                    rate = culprit_core::format::count(20.0 / 3602.0 * 3600.0)
                ),
            ]
            .join(" ")
        );
        assert_eq!(changed.investigation, culprit_core::t!("view.compilation.comparison.investigate.cadence"));
        assert_eq!(changed.verify, culprit_core::t!("view.compilation.comparison.verify"));
        let entry = &analysis.report.entries[0];
        assert_eq!(entry.identity, changed.identity);
        let statement = entry.statement.as_ref().unwrap();
        assert_eq!(statement.unavailable, (!available).then_some(PayloadUnavailable::Restricted));
        match expected_statement {
            DefinitionPayload::Available { text, .. } => {
                assert_eq!(statement.preview.as_deref(), Some(text.as_str()));
                assert!(!text.contains("9264"), "the original SQL payload already owns literal redaction");
            }
            DefinitionPayload::Unavailable { .. } => assert!(statement.preview.is_none()),
        }
        let TelemetryRecord::Operational(OperationalObservation {
            fact: OperationalFact::MssqlQueryCompilation(recorded),
            ..
        }) = &analysis.context.records[0].row.record
        else {
            panic!()
        };
        assert_eq!(recorded.memory, expected_memory);
        for (metric, value) in [
            ("common.metric.compile_memory_average", expected_memory.as_ref().unwrap().average_kb.unwrap()),
            ("common.metric.compile_memory_last", expected_memory.as_ref().unwrap().last_kb.unwrap()),
            ("common.metric.compile_memory_maximum", expected_memory.as_ref().unwrap().maximum_kb.unwrap()),
        ] {
            assert_eq!(
                entry.metrics.iter().find(|m| m.is(metric)).unwrap().text.as_deref(),
                Some(
                    culprit_core::t!("common.bytes.kib", n = culprit_core::format::digits(value.to_string())).as_str()
                )
            );
        }
        assert_eq!(a.metrics().recovery_objects + b.metrics().recovery_objects, 0);
    }
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
