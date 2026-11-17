use super::fixture::Fixture;
use culprit_collector::analytical::{
    evidence::{EvidenceFamily, RecoveryPayloadBudget},
    test_support::completed_spill_evidence_cases,
};
use culprit_core::{CompletedSpillState, CompletedSpillStatementBinding, SourceState, telemetry::*};
use culprit_engine::collected_resources;

const RUNTIME: &[EvidenceFamily] = &[EvidenceFamily::OperationalSamples];
const DEFINITIONS: &[EvidenceFamily] = &[EvidenceFamily::ObjectDefinitions];

#[tokio::test]
async fn completed_spills_use_native_incarnations_and_keep_each_original_context_and_receipt() {
    for (index, case) in completed_spill_evidence_cases().into_iter().enumerate() {
        let runtime = Fixture::with_sequence(case.runtime.0.clone(), case.runtime.1, 600_000 + index as u64 * 2).await;
        let runtime_opens = runtime.recovery_object_reads();
        let capture = runtime.read_families(RUNTIME).await;
        let definition = match case.definitions {
            Some((source, records)) => Some(Fixture::with_sequence(source, records, 600_001 + index as u64 * 2).await),
            None => None,
        };
        let definition_opens = definition.as_ref().map(Fixture::recovery_object_reads);
        let definitions = match &definition {
            Some(fixture) if case.name == "short_unicode_sql" => {
                Some(fixture.read_families_with_recovery(DEFINITIONS, RecoveryPayloadBudget::default()).await)
            }
            Some(fixture) => Some(fixture.read_families(DEFINITIONS).await),
            None => None,
        };
        let result = collected_resources::completed_spills(&capture, definitions.as_ref()).unwrap();
        assert_eq!(result.context.source, case.runtime.0, "{}", case.name);
        assert_eq!(result.context.capture, *capture.capture());
        if let Some(definitions) = &definitions {
            let original = result.definition_context.as_ref().unwrap();
            assert_eq!(original.capture, *definitions.capture());
            assert_eq!(original.source, *definitions.source());
            assert_ne!(original.capture, result.context.capture);
        } else {
            assert!(result.definition_context.is_none());
        }
        if let Some(state) = case.state {
            assert_eq!(result.report.entries.len(), 1, "{}", case.name);
            let entry = &result.report.entries[0];
            assert_eq!(entry.state, state, "{}", case.name);
            assert_eq!(Some(entry.statement_binding), case.binding, "{}", case.name);
            assert_eq!(entry.observation.captured().unwrap().capture, *capture.capture());
            assert_eq!(entry.observation.captured().unwrap().manifest_sha256, capture.capture_manifest_sha256());
            if let Some(reference) = &entry.definition {
                assert_eq!(reference.captured().unwrap().capture, *definitions.as_ref().unwrap().capture());
            }
            if state == CompletedSpillState::RecordedSpills {
                assert_eq!(
                    entry
                        .metrics
                        .iter()
                        .find(|metric| metric.is("view.completed_spills.metric.total"))
                        .unwrap()
                        .text
                        .as_deref(),
                    Some("9007199254740993")
                );
            }
            if case.name == "monitoring" {
                assert_eq!(entry.origin, StatementOrigin::CulpritMonitoring);
                assert_eq!(entry.monitoring_template_id, Some(digest(b"native-monitor-template")));
            }
            if case.name == "restricted_sql" {
                assert_eq!(entry.statement_binding, CompletedSpillStatementBinding::Matched);
                assert_eq!(entry.statement_unavailable, Some(PayloadUnavailable::Restricted));
                assert!(entry.statement_preview.is_none());
            }
            assert!(!entry.statement_preview.as_ref().is_some_and(|text| text.contains("9264")));
        } else {
            assert!(result.report.entries.is_empty());
        }
        assert_eq!(runtime.recovery_object_reads(), runtime_opens);
        assert_eq!(capture.metrics().recovery_objects, 0);
        if let Some(fixture) = &definition {
            let hydrated_objects = usize::from(case.name == "short_unicode_sql");
            assert_eq!(fixture.recovery_object_reads(), definition_opens.unwrap() + hydrated_objects);
            assert_eq!(definitions.as_ref().unwrap().metrics().recovery_objects, hydrated_objects as u32);
        }
    }
}

#[tokio::test]
async fn missing_runtime_and_definition_rows_preserve_gaps_without_false_zero_or_borrowed_sql() {
    for missing_runtime in [false, true] {
        let case = completed_spill_evidence_cases().into_iter().find(|case| case.name == "recorded_spills").unwrap();
        let runtime = Fixture::with_sequence(case.runtime.0, case.runtime.1, 700_000).await;
        let (source, records) = case.definitions.unwrap();
        let definition = Fixture::with_sequence(source, records, 700_001).await;
        let runtime_opens = runtime.recovery_object_reads();
        let definition_opens = definition.recovery_object_reads();
        if missing_runtime {
            runtime.remove_family(TelemetryStream::OperationalSamples);
        } else {
            definition.remove_family(TelemetryStream::ObjectDefinitions);
        }
        let capture = runtime.read_families(RUNTIME).await;
        let definitions = definition.read_families(DEFINITIONS).await;
        let result = collected_resources::completed_spills(&capture, Some(&definitions)).unwrap();
        if missing_runtime {
            assert_eq!((result.report.native_rows, result.report.observed_rows), (1, 0));
            assert!(result.report.entries.is_empty());
            assert_eq!(result.report.sources[0].state, SourceState::Partial);
            assert!(!result.context.read_gaps.is_empty());
        } else {
            let entry = &result.report.entries[0];
            assert_eq!(entry.state, CompletedSpillState::RecordedSpills);
            assert_eq!(entry.statement_binding, CompletedSpillStatementBinding::DefinitionNotObserved);
            assert!(entry.statement_preview.is_none());
            assert_eq!(result.report.definition_sources[0].state, SourceState::Partial);
            assert!(!result.definition_context.as_ref().unwrap().read_gaps.is_empty());
        }
        assert_eq!(runtime.recovery_object_reads(), runtime_opens);
        assert_eq!(definition.recovery_object_reads(), definition_opens);
        assert_eq!(capture.metrics().recovery_objects, 0);
        assert_eq!(definitions.metrics().recovery_objects, 0);
    }
}

#[tokio::test]
async fn a_short_multibyte_warehouse_preview_is_never_mistaken_for_complete_statement_sql() {
    let case = completed_spill_evidence_cases().into_iter().find(|case| case.name == "short_unicode_sql").unwrap();
    let runtime = Fixture::with_sequence(case.runtime.0, case.runtime.1, 800_000).await;
    let runtime_opens = runtime.recovery_object_reads();
    let capture = runtime.read_families(RUNTIME).await;
    assert_eq!(runtime.recovery_object_reads(), runtime_opens);
    let (source, records) = case.definitions.unwrap();
    let TelemetryRecord::ObjectDefinition(record) = &records[0] else { panic!() };
    let ObjectDefinitionFact::MssqlCachedDefinition(native) = &record.fact else { panic!() };
    let DefinitionPayload::Available { text, .. } = &native.statement.payload else { panic!() };
    assert!(text.chars().count() < 512 && text.len() > 1024);
    let definition = Fixture::with_sequence(source, records, 800_001).await;
    let definition_opens = definition.recovery_object_reads();
    let preview = definition.read_families(DEFINITIONS).await;
    assert!(!preview.is_read_complete());
    assert!(preview.records().is_empty());
    let result = collected_resources::completed_spills(&capture, Some(&preview)).unwrap();
    assert_eq!(result.report.entries[0].statement_binding, CompletedSpillStatementBinding::DefinitionNotObserved);
    assert!(result.report.entries[0].statement_preview.is_none());
    assert!(!result.definition_context.as_ref().unwrap().read_gaps.is_empty());
    assert_eq!(definition.recovery_object_reads(), definition_opens);
    assert_eq!(preview.metrics().recovery_objects, 0);
    let batch = &definition.range.manifest().batches[0];
    let hydrated = definition
        .read_families_with_recovery(DEFINITIONS, RecoveryPayloadBudget { max_objects: 1, max_bytes: batch.bytes })
        .await;
    assert!(hydrated.is_read_complete());
    let result = collected_resources::completed_spills(&capture, Some(&hydrated)).unwrap();
    let entry = &result.report.entries[0];
    assert_eq!(entry.statement_binding, CompletedSpillStatementBinding::Matched);
    assert!(entry.statement_preview.is_some());
    assert!(!entry.statement_preview_truncated);
    assert_eq!(hydrated.metrics().recovery_objects, 1);
    assert_eq!(definition.recovery_object_reads(), definition_opens + 1);
}

#[tokio::test]
async fn completed_original_capture_produces_distinct_cache_incarnations_without_a_hash_join_or_another_read() {
    let case = completed_spill_evidence_cases().into_iter().find(|case| case.name == "recorded_spills").unwrap();
    let (mut source, mut rows) = case.runtime;
    let mut second = rows[0].clone();
    let TelemetryRecord::Operational(OperationalObservation {
        key,
        fact: OperationalFact::MssqlCachedRuntime(runtime),
        ..
    }) = &mut second
    else {
        panic!()
    };
    runtime.context.identity.plan_generation =
        (runtime.context.identity.plan_generation.parse::<u64>().unwrap() + 1).to_string();
    let pages = runtime.counters.get_mut(&CachedRuntimeMetric::SpilledPages).unwrap();
    pages.total = "0".into();
    pages.last = "0".into();
    pages.minimum = "0".into();
    pages.maximum = "0".into();
    *key = runtime.context.identity.key().unwrap();
    rows.push(second);
    let SourceObservation::Snapshot(header) = &mut source.observation else { panic!() };
    header.records = 2;
    header.sources[0].records = 2;
    let fixture = Fixture::with_sequence(source.clone(), rows, 800_100).await;
    let capture = fixture.read_families(RUNTIME).await;
    let opens = fixture.recovery_object_reads();
    let original = serde_json::to_value(capture.records()).unwrap();
    let analyzed = collected_resources::completed_spills(&capture, None).unwrap();
    assert_eq!(analyzed.report.entries.len(), 2);
    assert_eq!(
        analyzed.report.entries.iter().filter(|entry| entry.state == CompletedSpillState::RecordedSpills).count(),
        1
    );
    assert_eq!(
        analyzed.report.entries.iter().filter(|entry| entry.state == CompletedSpillState::NoRecordedSpills).count(),
        1
    );
    assert_ne!(analyzed.report.entries[0].identity, analyzed.report.entries[1].identity);
    assert_eq!(
        analyzed.context.records.len(),
        2,
        "the result retains original raw records once, without a second carrier copy"
    );
    for entry in &analyzed.report.entries {
        let receipt = entry.observation.captured().expect("authenticated SDK provenance stays captured");
        assert_eq!(receipt.capture, *capture.capture());
        assert_eq!(receipt.manifest_sha256, capture.capture_manifest_sha256());
    }
    assert_eq!(serde_json::to_value(capture.records()).unwrap(), original);
    assert_eq!(fixture.recovery_object_reads(), opens);
}
