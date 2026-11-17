use super::fixture::Fixture;
use culprit_collector::analytical::{
    evidence::{EvidenceFamily, RecoveryPayloadBudget},
    test_support::compilation_definition_evidence_cases,
};
use culprit_core::{query_store_statement_report::QueryStoreStatementBinding, telemetry::*};
use culprit_engine::collected_resources;

const COUNTERS: &[EvidenceFamily] = &[EvidenceFamily::OperationalSamples];
const DEFINITIONS: &[EvidenceFamily] = &[EvidenceFamily::QueryDefinitions];

#[tokio::test]
async fn original_definitions_recover_supported_compile_policy_without_replacing_source_or_baseline_proofs() {
    for (index, case) in compilation_definition_evidence_cases().into_iter().enumerate() {
        let offset = 900_000 + index as u64 * 3;
        let before = Fixture::with_sequence(case.baseline.0, case.baseline.1, offset).await;
        let current = Fixture::with_sequence(case.current.0.clone(), case.current.1, offset + 1).await;
        let before_opens = before.recovery_object_reads();
        let current_opens = current.recovery_object_reads();
        let baseline = before.read_families(COUNTERS).await;
        let capture = current.read_families(COUNTERS).await;
        let definition_fixture = match case.definitions {
            Some((source, records)) => Some(Fixture::with_sequence(source, records, offset + 2).await),
            None => None,
        };
        let definition_opens = definition_fixture.as_ref().map(Fixture::recovery_object_reads);
        let definitions = match &definition_fixture {
            Some(fixture) if case.name == "short_unicode_sql" => {
                let batch = fixture
                    .range
                    .manifest()
                    .batches
                    .iter()
                    .find(|batch| batch.stream == TelemetryStream::QueryDefinitions)
                    .unwrap();
                Some(
                    fixture
                        .read_families_with_recovery(
                            DEFINITIONS,
                            RecoveryPayloadBudget { max_objects: 1, max_bytes: batch.bytes },
                        )
                        .await,
                )
            }
            Some(fixture) => Some(fixture.read_families(DEFINITIONS).await),
            None => None,
        };
        let result = collected_resources::compilation(&capture, Some(&baseline), definitions.as_ref()).unwrap();
        assert_eq!(result.context.source, case.current.0, "{}", case.name);
        assert_eq!(result.context.capture, *capture.capture());
        assert_eq!(result.baseline_context.as_ref().unwrap().capture, *baseline.capture());
        assert_eq!(result.context.capture_manifest_sha256, capture.capture_manifest_sha256());
        assert_eq!(
            result.baseline_context.as_ref().unwrap().capture_manifest_sha256,
            baseline.capture_manifest_sha256()
        );
        let entry = &result.report.entries[0];
        assert_eq!(entry.statement.as_ref().map(|statement| statement.binding), case.binding, "{}", case.name);
        assert_eq!(
            entry.metrics.iter().any(|metric| metric.is("view.compilation.metric.lifetime_duration")),
            case.expensive_directive,
            "{}",
            case.name
        );
        let TelemetryRecord::Operational(OperationalObservation {
            fact: OperationalFact::MssqlQueryCompilation(native),
            ..
        }) = &capture.records()[0].row().record
        else {
            panic!()
        };
        assert_eq!(native.origin, StatementOrigin::Unknown);
        assert_eq!(native.statement, DefinitionPayload::Unavailable { reason: PayloadUnavailable::NotCollected });
        assert_eq!(
            entry.metrics.iter().find(|metric| metric.is("view.compilation.metric.count")).unwrap().text.as_deref(),
            Some(native.compilations.as_str())
        );
        if let Some(definitions) = &definitions {
            let context = result.definition_context.as_ref().unwrap();
            assert_eq!(context.source, *definitions.source());
            assert_eq!(context.capture, *definitions.capture());
            assert_eq!(context.capture_manifest_sha256, definitions.capture_manifest_sha256());
            assert_eq!(context.collected_from, definitions.collected_from());
            assert_eq!(context.collected_until, definitions.collected_until());
            if let Some(reference) = entry.statement.as_ref().and_then(|statement| statement.definition.as_ref()) {
                assert_eq!(reference.capture, *definitions.capture());
                assert_eq!(reference.manifest_sha256, definitions.capture_manifest_sha256());
            }
        } else {
            assert!(result.definition_context.is_none() && result.report.definition_read.is_none());
        }
        assert_eq!(before.recovery_object_reads(), before_opens);
        assert_eq!(current.recovery_object_reads(), current_opens);
        if let Some(fixture) = definition_fixture {
            assert_eq!(
                fixture.recovery_object_reads(),
                definition_opens.unwrap() + usize::from(case.name == "short_unicode_sql")
            );
        }
        let encoded = serde_json::to_value(&result.report).unwrap();
        assert!(encoded["entries"][0].get("memory_total").is_none());
        assert!(encoded["entries"][0].get("current_compiler").is_none());
        assert!(
            !entry
                .statement
                .as_ref()
                .and_then(|statement| statement.preview.as_ref())
                .is_some_and(|text| text.contains("81724"))
        );
    }
}

#[tokio::test]
async fn missing_definition_rows_preserve_original_counters_and_read_gaps_without_representative_sql() {
    let case = compilation_definition_evidence_cases().into_iter().find(|case| case.name == "matched").unwrap();
    let current = Fixture::with_sequence(case.current.0, case.current.1, 910_000).await;
    let (source, records) = case.definitions.unwrap();
    let definition = Fixture::with_sequence(source, records, 910_001).await;
    let opens = definition.recovery_object_reads();
    definition.remove_family(TelemetryStream::QueryDefinitions);
    let capture = current.read_families(COUNTERS).await;
    let definitions = definition.read_families(DEFINITIONS).await;
    let result = collected_resources::compilation(&capture, None, Some(&definitions)).unwrap();
    assert_eq!(result.report.entries[0].statement.as_ref().unwrap().binding, QueryStoreStatementBinding::NotObserved);
    assert!(result.report.entries[0].statement.as_ref().unwrap().preview.is_none());
    let selected = result.report.definition_read.unwrap();
    assert_eq!((selected.selected_rows, selected.observed_rows), (1, 0));
    assert!(!selected.read_complete);
    assert!(!result.definition_context.unwrap().read_gaps.is_empty());
    assert_eq!(definition.recovery_object_reads(), opens);
    assert_eq!(definitions.metrics().recovery_objects, 0);
}

#[tokio::test]
async fn short_unicode_previews_cannot_supply_policy_before_exact_native_definition_hydration() {
    let case =
        compilation_definition_evidence_cases().into_iter().find(|case| case.name == "short_unicode_sql").unwrap();
    let current = Fixture::with_sequence(case.current.0, case.current.1, 920_000).await;
    let capture = current.read_families(COUNTERS).await;
    let (source, records) = case.definitions.unwrap();
    let TelemetryRecord::QueryDefinition(native) = &records[0] else { panic!() };
    let DefinitionPayload::Available { text, .. } = &native.sql else { panic!() };
    assert!(text.chars().count() < 512 && text.len() > 1024);
    let fixture = Fixture::with_sequence(source, records, 920_001).await;
    let opens = fixture.recovery_object_reads();
    let preview = fixture.read_families(DEFINITIONS).await;
    assert!(!preview.is_read_complete() && preview.records().is_empty());
    let result = collected_resources::compilation(&capture, None, Some(&preview)).unwrap();
    assert_eq!(result.report.entries[0].statement.as_ref().unwrap().binding, QueryStoreStatementBinding::NotObserved);
    assert!(
        !result.report.entries[0].metrics.iter().any(|metric| metric.is("view.compilation.metric.lifetime_duration"))
    );
    assert_eq!(fixture.recovery_object_reads(), opens);
    let batch = fixture
        .range
        .manifest()
        .batches
        .iter()
        .find(|batch| batch.stream == TelemetryStream::QueryDefinitions)
        .unwrap();
    let hydrated = fixture
        .read_families_with_recovery(DEFINITIONS, RecoveryPayloadBudget { max_objects: 1, max_bytes: batch.bytes })
        .await;
    let result = collected_resources::compilation(&capture, None, Some(&hydrated)).unwrap();
    let statement = result.report.entries[0].statement.as_ref().unwrap();
    assert_eq!(statement.binding, QueryStoreStatementBinding::Matched);
    assert!(!statement.preview_truncated);
    assert!(statement.preview.as_ref().unwrap().contains("OPTION(RECOMPILE)"));
    assert_eq!(fixture.recovery_object_reads(), opens + 1);
    assert_eq!(hydrated.metrics().recovery_objects, 1);
}
