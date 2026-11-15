use super::*;
use culprit_collector::analytical::{
    evidence::{EvidenceFamily, EvidenceGapReason, RecoveryPayloadBudget},
    test_support::{self, CacheEntryMemoryCase},
};
use culprit_core::{ErrorDetail, SourceOutcome, SourceState};
use culprit_engine::collected_resources;

const FAMILY: &[EvidenceFamily] = &[EvidenceFamily::OperationalSamples];

fn observations(records: &[TelemetryRecord]) -> Vec<&OperationalObservation> {
    records
        .iter()
        .map(|record| {
            let TelemetryRecord::Operational(observation) = record else { panic!() };
            observation
        })
        .collect()
}

#[tokio::test]
async fn authenticated_cache_objects_reuse_the_pure_view_and_preserve_exact_focus_and_original_coverage() {
    for (_, source, records) in test_support::cache_entry_memory_evidence_cases() {
        let native = source.observation.snapshot().unwrap();
        let outcome = SourceOutcome {
            source: native.sources[0].source.clone(),
            state: native.sources[0].state,
            reason: None,
            reason_message: None,
        };
        let context = NativeReadContext::from_snapshot(&source).unwrap();
        let expected =
            culprit_checks::cache_entry_memory::explain(&context, &observations(&records), Some(&outcome)).unwrap();
        let fixture = Fixture::new(source.clone(), records.clone()).await;
        let before = fixture.recovery_object_reads();
        let capture = fixture.read_families(FAMILY).await;
        test_support::assert_cache_entry_memory_capture(&capture, &source, &records);
        let analysis = collected_resources::cache_entry_memory(&capture).unwrap();
        assert_eq!(serde_json::to_value(&analysis.report).unwrap(), serde_json::to_value(expected).unwrap());
        assert_eq!(analysis.context.source, source);
        assert_eq!(analysis.context.capture, *capture.capture());
        assert_eq!(analysis.context.records.len(), records.len());
        assert!(analysis.context.read_gaps.is_empty());
        for record in records {
            let row = ExportedRecord::new(&source, capture.capture(), record).unwrap();
            assert_eq!(analysis.context.records.iter().filter(|actual| actual.row == row).count(), 1);
        }
        assert_eq!(fixture.recovery_object_reads(), before);
        let encoded = serde_json::to_value(&analysis).unwrap();
        assert!(encoded.get("workload").is_none() && encoded.get("findings").is_none());
    }
}

#[tokio::test]
async fn a_multi_statement_object_reports_one_whole_footprint_without_charging_each_statement() {
    let (source, records) = test_support::cache_entry_memory_evidence(CacheEntryMemoryCase::Complete);
    let fixture = Fixture::new(source, records).await;
    let capture = fixture.read_families(FAMILY).await;
    let analysis = collected_resources::cache_entry_memory(&capture).unwrap();
    assert_eq!(analysis.report.entries.len(), 1);
    assert_eq!(analysis.context.records.len(), 1);
    let TelemetryRecord::Operational(original) = &analysis.context.records[0].row.record else { panic!() };
    let OperationalFact::MssqlCacheEntryMemory(entry) = &original.fact else { panic!() };
    assert_eq!(entry.statements.len(), 2);
    assert_eq!(analysis.report.entries[0].observation_key, original.key);
    let metrics = &analysis.report.entries[0].metrics;
    assert_eq!(metrics.len(), 3);
    assert_eq!(metrics.iter().filter(|metric| metric.is("view.cache_entry_memory.metric.bytes")).count(), 1);
    for (key, exact) in [
        ("view.cache_entry_memory.metric.bytes", "9007199254740993"),
        ("view.cache_entry_memory.metric.lookups", "40"),
        ("view.cache_entry_memory.metric.references", "0"),
    ] {
        assert_eq!(metrics.iter().find(|metric| metric.is(key)).unwrap().text.as_deref(), Some(exact));
    }
    assert!(analysis.report.context_differences.is_empty());
    assert!(!analysis.report.limitations.is_empty());
}

#[tokio::test]
async fn unknown_cache_keys_and_unavailable_sql_keep_the_original_positive_object_without_a_context_claim() {
    for case in [CacheEntryMemoryCase::UnknownCacheKeys, CacheEntryMemoryCase::MissingSql] {
        let (source, records) = test_support::cache_entry_memory_evidence(case);
        let fixture = Fixture::new(source.clone(), records).await;
        let capture = fixture.read_families(FAMILY).await;
        let analysis = collected_resources::cache_entry_memory(&capture).unwrap();
        assert_eq!(analysis.report.source_state, Some(SourceState::Collected));
        assert_eq!(analysis.report.entries.len(), 1);
        assert!(analysis.report.context_differences.is_empty());
        assert_eq!(analysis.context.source, source);
        let TelemetryRecord::Operational(row) = &analysis.context.records[0].row.record else { panic!() };
        let OperationalFact::MssqlCacheEntryMemory(entry) = &row.fact else { panic!() };
        match case {
            CacheEntryMemoryCase::UnknownCacheKeys => {
                assert!(entry.cache_keys[0].native_type.is_none() && entry.cache_keys[0].value.is_none());
            }
            CacheEntryMemoryCase::MissingSql => {
                assert_eq!(
                    entry.statements[0].statement,
                    DefinitionPayload::Unavailable { reason: PayloadUnavailable::Restricted }
                );
                assert_eq!(
                    entry.statements[1].statement,
                    DefinitionPayload::Unavailable { reason: PayloadUnavailable::NotCollected }
                );
            }
            _ => unreachable!(),
        }
    }
}

#[tokio::test]
async fn partial_empty_and_unavailable_native_coverage_remain_distinct_from_reader_completeness() {
    for (case, expected) in [
        (CacheEntryMemoryCase::Partial, SourceState::Partial),
        (CacheEntryMemoryCase::Empty, SourceState::Empty),
        (CacheEntryMemoryCase::UnavailableSource, SourceState::Unavailable),
    ] {
        let (source, records) = test_support::cache_entry_memory_evidence(case);
        let fixture = Fixture::new(source.clone(), records.clone()).await;
        let capture = fixture.read_families(FAMILY).await;
        assert!(capture.is_read_complete());
        let analysis = collected_resources::cache_entry_memory(&capture).unwrap();
        assert_eq!(analysis.report.source_state, Some(expected));
        assert_eq!(analysis.report.entries.len(), records.len());
        assert_eq!(analysis.context.source, source);
        if case == CacheEntryMemoryCase::UnavailableSource {
            assert_eq!(
                analysis.context.source.observation.snapshot().unwrap().sources[0].error_code,
                Some(culprit_core::ErrorCode::PermissionDenied)
            );
        }
        assert!(!analysis.report.limitations.is_empty(), "a selected cache read never proves workload absence");
    }
}

#[tokio::test]
async fn a_lost_row_or_unpublished_capture_cannot_turn_selected_cache_context_into_a_complete_empty_result() {
    for unpublished in [false, true] {
        let (source, records) = test_support::cache_entry_memory_evidence(CacheEntryMemoryCase::Complete);
        let fixture = Fixture::new(source.clone(), records).await;
        if unpublished {
            fixture.unpublish();
        } else {
            fixture.remove_first_row(TelemetryStream::OperationalSamples);
        }
        let before = fixture.recovery_object_reads();
        let capture = fixture.read_families(FAMILY).await;
        let analysis = collected_resources::cache_entry_memory(&capture).unwrap();
        assert!(analysis.report.entries.is_empty());
        assert_eq!(analysis.report.source_state, Some(SourceState::Partial));
        assert_eq!(analysis.context.source, source);
        assert_eq!(analysis.context.source.observation.snapshot().unwrap().sources[0].state, SourceState::Collected);
        let expected = if unpublished { EvidenceGapReason::NotPublished } else { EvidenceGapReason::MissingRows };
        assert!(analysis.context.read_gaps.iter().any(|gap| gap.reason == expected));
        assert_eq!(fixture.recovery_object_reads(), before);
    }
}

#[tokio::test]
async fn explicit_recovery_restores_eight_sql_payloads_and_one_whole_entry_without_new_scope_authority() {
    let (source, records) = test_support::cache_entry_memory_evidence(CacheEntryMemoryCase::EightStatements);
    let fixture = Fixture::new(source.clone(), records.clone()).await;
    let before = fixture.recovery_object_reads();
    let capture = fixture.read_families(FAMILY).await;
    let preview = collected_resources::cache_entry_memory(&capture).unwrap();
    assert!(preview.report.entries.is_empty());
    assert_eq!(preview.report.source_state, Some(SourceState::Partial));
    assert!(preview.context.read_gaps.iter().any(|gap| gap.reason == EvidenceGapReason::DetailInRecovery));
    assert_eq!(fixture.recovery_object_reads(), before);
    let capture = fixture.read_families_with_recovery(FAMILY, RecoveryPayloadBudget::default()).await;
    test_support::assert_cache_entry_memory_capture(&capture, &source, &records);
    let restored = collected_resources::cache_entry_memory(&capture).unwrap();
    assert_eq!(restored.report.entries.len(), 1);
    assert_eq!(restored.report.source_state, Some(SourceState::Collected));
    assert_eq!(restored.context.source, source);
    assert_eq!(restored.context.records.len(), 1);
    assert_eq!(restored.context.records[0].row.record.definition_payloads().count(), MAX_CACHE_ENTRY_STATEMENTS);
    assert_eq!(capture.metrics().recovery_objects, 1);
    assert_eq!(capture.metrics().recovery_bytes, fixture.range.manifest().batches[0].bytes);
    assert_eq!(fixture.recovery_object_reads(), before + 1);
}

#[tokio::test]
async fn unrelated_native_datasets_and_missing_selected_family_cannot_be_read_as_cache_memory() {
    let (source, records) = test_support::statement_grant_evidence(test_support::StatementGrantCase::Complete);
    let fixture = Fixture::new(source, records).await;
    let wrong = fixture.read_families(FAMILY).await;
    assert_eq!(
        collected_resources::cache_entry_memory(&wrong).unwrap_err().detail,
        Some(ErrorDetail::Delivery(DeliveryProblem::UnsupportedSchema))
    );
    let (source, records) = test_support::cache_entry_memory_evidence(CacheEntryMemoryCase::Complete);
    let fixture = Fixture::new(source, records).await;
    let wrong = fixture.read_families(&[EvidenceFamily::TuningObservations]).await;
    assert_eq!(
        collected_resources::cache_entry_memory(&wrong).unwrap_err().detail,
        Some(ErrorDetail::Delivery(DeliveryProblem::UnsupportedSchema))
    );
}
