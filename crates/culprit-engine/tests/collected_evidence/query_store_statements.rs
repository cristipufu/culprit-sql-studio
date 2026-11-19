use super::fixture::Fixture;
use culprit_collector::analytical::{
    evidence::{EvidenceFamily, RecoveryPayloadBudget},
    test_support::query_store_statement_evidence_cases,
};
use culprit_core::{query_store_statement_report::QueryStoreStatementBinding, telemetry::*};
use culprit_engine::collected_resources;
const RUNTIME: &[EvidenceFamily] = &[EvidenceFamily::QueryRuntime];
const DEFINITIONS: &[EvidenceFamily] = &[EvidenceFamily::QueryDefinitions];

#[tokio::test]
async fn original_definition_cases_have_the_same_typed_binding_in_compiler_and_page_reports() {
    for (index, case) in query_store_statement_evidence_cases().into_iter().enumerate() {
        let sequence = 950_000 + index as u64 * 3;
        let runtime_fixture = Fixture::with_sequence(case.runtime.0.clone(), case.runtime.1, sequence).await;
        let compiler_fixture = Fixture::with_sequence(case.compilation.0, case.compilation.1, sequence + 1).await;
        let definitions_fixture = if let Some((source, records)) = case.definitions {
            Some(Fixture::with_sequence(source, records, sequence + 2).await)
        } else {
            None
        };
        let runtime_opens = runtime_fixture.recovery_object_reads();
        let compiler_opens = compiler_fixture.recovery_object_reads();
        let definition_opens = definitions_fixture.as_ref().map(Fixture::recovery_object_reads);
        let runtime = runtime_fixture.read_families(RUNTIME).await;
        let compiler = compiler_fixture.read_families(&[EvidenceFamily::OperationalSamples]).await;
        let definitions = if let Some(fixture) = &definitions_fixture {
            Some(fixture.read_families(DEFINITIONS).await)
        } else {
            None
        };
        let pages = collected_resources::page_server_reads(&runtime, definitions.as_ref()).unwrap();
        let compilation = collected_resources::compilation(&compiler, None, definitions.as_ref()).unwrap();
        let expected =
            if case.name == "short_unicode_sql" { Some(QueryStoreStatementBinding::NotObserved) } else { case.binding };
        assert_eq!(
            pages.report.entries[0].statement.as_ref().map(|statement| statement.binding),
            expected,
            "{}",
            case.name
        );
        assert_eq!(
            compilation.report.entries[0].statement.as_ref().map(|statement| statement.binding),
            expected,
            "{}",
            case.name
        );
        assert_eq!(pages.report.entries[0].executions, "17");
        assert!(
            pages.report.entries[0]
                .explanation
                .metrics
                .iter()
                .any(|metric| metric.text.as_deref() == Some("9007199254740993"))
        );
        assert_eq!(pages.context.source, case.runtime.0);
        assert!(pages.report.read_complete);
        if let Some(definitions) = &definitions {
            let context = pages.definition_context.as_ref().unwrap();
            assert_eq!(&context.source, definitions.source());
            assert_eq!(context.records.len(), definitions.records().len());
            assert_eq!(context.collected_from, definitions.collected_from());
            assert_eq!(context.collected_until, definitions.collected_until());
            assert_eq!(pages.report.definition_read.as_ref().unwrap().read_complete, definitions.is_read_complete());
        } else {
            assert!(pages.definition_context.is_none() && pages.report.definition_read.is_none());
        }
        assert_eq!(runtime_fixture.recovery_object_reads(), runtime_opens);
        assert_eq!(compiler_fixture.recovery_object_reads(), compiler_opens);
        if let Some(fixture) = definitions_fixture {
            assert_eq!(fixture.recovery_object_reads(), definition_opens.unwrap());
        }
    }
}

#[tokio::test]
async fn an_explicit_same_capture_definition_family_uses_the_identical_original_receipt_and_clocks() {
    let case = query_store_statement_evidence_cases().into_iter().find(|case| case.name == "matched").unwrap();
    let fixture = Fixture::with_sequence(case.runtime.0, case.runtime.1, 951_000).await;
    let opens = fixture.recovery_object_reads();
    let runtime = fixture.read_families(RUNTIME).await;
    let definitions = fixture.read_families(DEFINITIONS).await;
    let analysis = collected_resources::page_server_reads(&runtime, Some(&definitions)).unwrap();
    let statement = analysis.report.entries[0].statement.as_ref().unwrap();
    assert_eq!(statement.binding, QueryStoreStatementBinding::Captured);
    assert!(statement.preview.is_some());
    assert_eq!(analysis.context.range_id, analysis.definition_context.as_ref().unwrap().range_id);
    assert_eq!(
        analysis.context.capture_manifest_sha256,
        analysis.definition_context.as_ref().unwrap().capture_manifest_sha256
    );
    assert_eq!(analysis.context.collected_from, analysis.definition_context.as_ref().unwrap().collected_from);
    assert_eq!(fixture.recovery_object_reads(), opens);
}

#[tokio::test]
async fn equal_range_cannot_borrow_a_different_authenticated_original_descriptor() {
    let case = query_store_statement_evidence_cases().into_iter().find(|case| case.name == "matched").unwrap();
    let mut different = case.runtime.0.clone();
    let SourceObservation::QueryHistory(history) = &mut different.observation else { panic!() };
    history.native_properties.insert("independent_original".into(), "different".into());
    let runtime = Fixture::with_sequence(case.runtime.0, case.runtime.1.clone(), 952_000).await;
    let definitions = Fixture::with_sequence(different, case.runtime.1, 952_000).await;
    let left = runtime.read_families(RUNTIME).await;
    let right = definitions.read_families(DEFINITIONS).await;
    assert_eq!(left.range_id(), right.range_id(), "same owned capture tuple under the same delivery contract");
    assert!(collected_resources::page_server_reads(&left, Some(&right)).is_err());
}

#[tokio::test]
async fn incomplete_definition_read_does_not_remove_original_pages_or_pick_representative_sql() {
    let case = query_store_statement_evidence_cases().into_iter().find(|case| case.name == "matched").unwrap();
    let runtime = Fixture::with_sequence(case.runtime.0, case.runtime.1, 953_000).await;
    let (source, records) = case.definitions.unwrap();
    let definitions = Fixture::with_sequence(source, records, 953_001).await;
    definitions.remove_family(TelemetryStream::QueryDefinitions);
    let opens = definitions.recovery_object_reads();
    let current = runtime.read_families(RUNTIME).await;
    let missing = definitions.read_families(DEFINITIONS).await;
    let analysis = collected_resources::page_server_reads(&current, Some(&missing)).unwrap();
    assert!(analysis.report.read_complete);
    assert_eq!(analysis.report.entries[0].executions, "17");
    assert_eq!(analysis.report.entries[0].statement.as_ref().unwrap().binding, QueryStoreStatementBinding::NotObserved);
    let read = analysis.report.definition_read.unwrap();
    assert_eq!((read.observed_rows, read.selected_rows), (0, 1));
    assert!(!read.read_complete);
    assert!(!analysis.definition_context.unwrap().read_gaps.is_empty());
    assert_eq!(definitions.recovery_object_reads(), opens);
}

#[tokio::test]
async fn a_short_unicode_preview_requires_explicit_bounded_hydration_before_sql_can_be_linked() {
    let case =
        query_store_statement_evidence_cases().into_iter().find(|case| case.name == "short_unicode_sql").unwrap();
    let runtime = Fixture::with_sequence(case.runtime.0, case.runtime.1, 954_000).await;
    let (source, records) = case.definitions.unwrap();
    let fixture = Fixture::with_sequence(source, records, 954_001).await;
    let opens = fixture.recovery_object_reads();
    let current = runtime.read_families(RUNTIME).await;
    let preview = fixture.read_families(DEFINITIONS).await;
    let analysis = collected_resources::page_server_reads(&current, Some(&preview)).unwrap();
    assert_eq!(analysis.report.entries[0].statement.as_ref().unwrap().binding, QueryStoreStatementBinding::NotObserved);
    assert!(analysis.report.entries[0].statement.as_ref().unwrap().preview.is_none());
    assert_eq!(fixture.recovery_object_reads(), opens);
    let batch = fixture
        .range
        .manifest()
        .batches
        .iter()
        .find(|batch| batch.stream == TelemetryStream::QueryDefinitions)
        .unwrap();
    let error = fixture
        .try_read_families_with_recovery(
            DEFINITIONS,
            RecoveryPayloadBudget { max_objects: 1, max_bytes: batch.bytes - 1 },
        )
        .await
        .unwrap_err();
    assert_eq!(error.detail, Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::Capacity)));
    assert_eq!(fixture.recovery_object_reads(), opens, "the original object size is admitted before any body download");
    let hydrated = fixture
        .read_families_with_recovery(DEFINITIONS, RecoveryPayloadBudget { max_objects: 1, max_bytes: batch.bytes })
        .await;
    let analysis = collected_resources::page_server_reads(&current, Some(&hydrated)).unwrap();
    let statement = analysis.report.entries[0].statement.as_ref().unwrap();
    assert_eq!(statement.binding, QueryStoreStatementBinding::Matched);
    assert!(!statement.preview_truncated);
    assert!(statement.preview.as_ref().unwrap().contains("OPTION(RECOMPILE)"));
    assert_eq!(hydrated.metrics().recovery_objects, 1);
    assert_eq!(fixture.recovery_object_reads(), opens + 1);
}

#[tokio::test]
async fn same_capture_binding_does_not_turn_a_unicode_warehouse_prefix_into_a_complete_sql_body() {
    let mut case =
        query_store_statement_evidence_cases().into_iter().find(|case| case.name == "short_unicode_sql").unwrap();
    let TelemetryRecord::QueryDefinition(original) = &case.definitions.as_ref().unwrap().1[0] else { panic!() };
    let TelemetryRecord::QueryDefinition(captured) =
        case.runtime.1.iter_mut().find(|record| record.stream() == TelemetryStream::QueryDefinitions).unwrap()
    else {
        panic!()
    };
    captured.sql = original.sql.clone();
    captured.source_text_bytes = original.source_text_bytes.clone();
    captured.origin = original.origin;
    let fixture = Fixture::with_sequence(case.runtime.0, case.runtime.1, 957_000).await;
    let opens = fixture.recovery_object_reads();
    let runtime = fixture.read_families(RUNTIME).await;
    let prefix = fixture.read_families(DEFINITIONS).await;
    let analysis = collected_resources::page_server_reads(&runtime, Some(&prefix)).unwrap();
    let statement = analysis.report.entries[0].statement.as_ref().unwrap();
    assert_eq!(statement.binding, QueryStoreStatementBinding::NotObserved);
    assert!(statement.preview.is_none() && statement.definition.is_none());
    assert!(analysis.report.read_complete);
    let read = analysis.report.definition_read.as_ref().unwrap();
    assert_eq!((read.selected_rows, read.observed_rows, read.read_complete), (1, 0, false));
    assert_eq!(fixture.recovery_object_reads(), opens);
    let bytes = fixture
        .range
        .manifest()
        .batches
        .iter()
        .find(|batch| batch.stream == TelemetryStream::QueryDefinitions)
        .unwrap()
        .bytes;
    let error = fixture
        .try_read_families_with_recovery(DEFINITIONS, RecoveryPayloadBudget { max_objects: 1, max_bytes: bytes - 1 })
        .await
        .unwrap_err();
    assert_eq!(error.detail, Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::Capacity)));
    assert_eq!(fixture.recovery_object_reads(), opens, "full original bytes are admitted before opening the body");
    let definitions = fixture
        .read_families_with_recovery(DEFINITIONS, RecoveryPayloadBudget { max_objects: 1, max_bytes: bytes })
        .await;
    let analysis = collected_resources::page_server_reads(&runtime, Some(&definitions)).unwrap();
    let statement = analysis.report.entries[0].statement.as_ref().unwrap();
    assert_eq!(statement.binding, QueryStoreStatementBinding::Captured);
    assert!(statement.definition.is_some());
    assert!(statement.preview.as_ref().unwrap().contains("OPTION(RECOMPILE)"));
    assert!(!statement.preview_truncated);
    assert_eq!(definitions.metrics().recovery_objects, 1);
    assert_eq!(fixture.recovery_object_reads(), opens + 1);
}
