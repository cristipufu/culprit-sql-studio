use super::fixture::Fixture;
use culprit_checks::memory_clerks::MemoryClerkEvidence;
use culprit_collector::analytical::{
    evidence::{EvidenceFamily, EvidenceGapReason},
    test_support::{self, MemoryClerkCase},
};
use culprit_core::{MeasurementScope, SourceState, telemetry::*};
use culprit_engine::collected_resources;

#[tokio::test]
async fn authentic_clerk_captures_use_one_pure_report_and_preserve_source_counts_nodes_and_allocation_unknowns() {
    for case in MemoryClerkCase::ALL {
        let (source, records) = test_support::memory_clerk_evidence(case);
        let fixture = Fixture::new(source.clone(), records.clone()).await;
        let opened = fixture.recovery_object_reads();
        let capture = fixture.read_families(&[EvidenceFamily::OperationalSamples]).await;
        test_support::assert_operational_capture(&capture, &source, &records);
        let analysis = collected_resources::memory_clerks(&capture).unwrap();
        assert_eq!(analysis.context.source, source);
        if case == MemoryClerkCase::SharedInstance {
            let CollectionTarget::Shared(instance) = &source.target else { panic!("native shared clerk source") };
            assert_eq!(instance.kind, SharedResourceKind::Instance);
            assert_eq!(source.target.database(), None);
            let Some(ObservationReadContext::Native { context }) = &analysis.report.context else {
                panic!("original native context")
            };
            assert_eq!(context.target, source.target);
            assert_eq!(context.partition, source.partition);
            assert_eq!(
                (context.observed_from.as_str(), context.observed_until.as_str()),
                ("2020-01-01T12:00:00.000000001Z", "2020-01-01T12:00:00.000000009Z")
            );
        }
        assert_eq!(analysis.context.records.len(), records.len());
        let coverage = &source.observation.snapshot().unwrap().sources[0];
        let context = NativeReadContext::from_snapshot(&source).unwrap();
        let rows = records
            .iter()
            .filter_map(|record| {
                let TelemetryRecord::Operational(row) = record else { panic!() };
                (row.source == culprit_core::source_id::MEMORY_CLERKS).then_some(row)
            })
            .collect::<Vec<_>>();
        let expected = culprit_checks::memory_clerks::explain(MemoryClerkEvidence::Captured {
            context: &context,
            coverage,
            observations: &rows,
            read_complete: true,
        })
        .unwrap();
        assert_eq!(serde_json::to_value(&analysis.report).unwrap(), serde_json::to_value(&expected).unwrap());
        assert_eq!(analysis.report.native_rows, Some(coverage.records));
        assert_eq!(analysis.report.native_source.as_ref(), Some(coverage));
        assert_eq!(analysis.report.observed_rows, coverage.records);
        assert_eq!(analysis.report.read_complete, Some(true));
        assert_eq!(analysis.report.source_state, Some(coverage.state));
        assert!(analysis.report.entries.iter().all(|entry| entry.scope == MeasurementScope::Server));
        if matches!(case, MemoryClerkCase::Exact | MemoryClerkCase::SharedInstance) {
            assert_eq!(analysis.report.entries.len(), 2);
            assert_ne!(analysis.report.entries[0].observation_key, analysis.report.entries[1].observation_key);
            assert!(analysis.report.entries.iter().all(|entry| entry.fields.len() == 9));
            for entry in &analysis.report.entries {
                assert_eq!(
                    entry.fields.iter().find(|field| field.key == "pages_kb").unwrap().value.as_deref(),
                    Some("9007199254740993")
                );
                assert_eq!(
                    entry.fields.iter().find(|field| field.key == "shared_memory_reserved_kb").unwrap().value,
                    None
                );
                assert_eq!(
                    entry
                        .fields
                        .iter()
                        .find(|field| field.key == "shared_memory_committed_kb")
                        .unwrap()
                        .value
                        .as_deref(),
                    Some("0")
                );
            }
        }
        if case == MemoryClerkCase::Unreported {
            assert!(
                analysis
                    .report
                    .entries
                    .iter()
                    .all(|entry| entry.fields.iter().all(|field| field.key == "clerk_count" || field.value.is_none()))
            );
        }
        if case == MemoryClerkCase::MeasuredZero {
            assert!(analysis.report.entries.iter().all(|entry| {
                entry
                    .fields
                    .iter()
                    .filter(|field| field.key != "clerk_count")
                    .all(|field| field.value.as_deref().is_none_or(|value| value == "0"))
            }));
        }
        assert_eq!(fixture.recovery_object_reads(), opened);
        assert_eq!(capture.metrics().recovery_objects, 0);
    }
}

#[tokio::test]
async fn missing_all_clerk_bodies_remains_a_gap_with_original_native_count_and_unrelated_rows() {
    let (source, records) = test_support::memory_clerk_evidence(MemoryClerkCase::Exact);
    let fixture = Fixture::new(source.clone(), records).await;
    fixture.remove_first_row(TelemetryStream::OperationalSamples);
    fixture.remove_first_row(TelemetryStream::OperationalSamples);
    let opened = fixture.recovery_object_reads();
    let capture = fixture.read_families(&[EvidenceFamily::OperationalSamples]).await;
    assert!(!capture.is_read_complete());
    assert!(capture.gaps().iter().any(|gap| gap.reason == EvidenceGapReason::MissingRows));
    let analysis = collected_resources::memory_clerks(&capture).unwrap();
    assert_eq!(analysis.context.source, source);
    assert_eq!(analysis.context.records.len(), 1);
    assert_eq!(analysis.report.native_rows, Some(2));
    assert_eq!(analysis.report.observed_rows, 0);
    assert_eq!(analysis.report.read_complete, Some(false));
    assert_eq!(analysis.report.source_state, Some(SourceState::Partial));
    assert!(analysis.report.entries.is_empty());
    assert_eq!(fixture.recovery_object_reads(), opened);
}

#[tokio::test]
async fn authenticated_but_malformed_native_clerk_semantics_fail_before_a_report_is_rendered() {
    for invalid_field in ["scope", "representation", "native-object", "reset"] {
        let (source, mut records) = test_support::memory_clerk_evidence(MemoryClerkCase::Exact);
        let TelemetryRecord::Operational(OperationalObservation { fact: OperationalFact::CounterSet(set), .. }) =
            &mut records[0]
        else {
            panic!()
        };
        match invalid_field {
            "scope" => {
                set.scope = MeasurementScope::Database;
                for counter in &mut set.counters {
                    counter.scope = MeasurementScope::Database;
                }
            }
            "representation" => set.counters[1].value = MeasurementValue::Real(42.0),
            "native-object" => set.counters[1].native_object = "another-native-view".into(),
            _ => set.counters[1].reset = culprit_core::ResetMarker::Generation("foreign-generation".into()),
        }
        assert!(set.is_valid());
        let fixture = Fixture::new(source, records).await;
        let opened = fixture.recovery_object_reads();
        let capture = fixture.read_families(&[EvidenceFamily::OperationalSamples]).await;
        assert!(collected_resources::memory_clerks(&capture).is_err(), "{invalid_field}");
        assert_eq!(fixture.recovery_object_reads(), opened);
    }
}

#[tokio::test]
async fn an_operational_capture_without_clerk_coverage_is_not_a_successful_empty_clerk_report() {
    let (mut source, mut records) = test_support::memory_clerk_evidence(MemoryClerkCase::Exact);
    records.retain(|record| matches!(record, TelemetryRecord::Operational(row) if row.source != culprit_core::source_id::MEMORY_CLERKS));
    let SourceObservation::Snapshot(snapshot) = &mut source.observation else { panic!() };
    snapshot.records = records.len() as u32;
    snapshot.sources.retain(|coverage| coverage.source != culprit_core::source_id::MEMORY_CLERKS);
    let fixture = Fixture::new(source, records).await;
    let opened = fixture.recovery_object_reads();
    let capture = fixture.read_families(&[EvidenceFamily::OperationalSamples]).await;
    let error = collected_resources::memory_clerks(&capture).unwrap_err();
    assert_eq!(error.detail, Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::UnsupportedSchema)));
    assert_eq!(fixture.recovery_object_reads(), opened);
}

#[tokio::test]
async fn client_serialization_preserves_the_shared_clerk_report_and_omits_an_absent_report() {
    let snapshot = serde_json::from_str(include_str!("../../../../fixtures/mssql-demo.json")).unwrap();
    let mut report = culprit_checks::analyze(snapshot);
    assert!(report.memory_clerks.is_none());
    assert!(serde_json::to_value(culprit_engine::reports::display(&report)).unwrap().get("memory_clerks").is_none());

    let (source, records) = test_support::memory_clerk_evidence(MemoryClerkCase::SharedInstance);
    let fixture = Fixture::new(source, records).await;
    let capture = fixture.read_families(&[EvidenceFamily::OperationalSamples]).await;
    let analysis = collected_resources::memory_clerks(&capture).unwrap();
    let expected = serde_json::to_value(&analysis.report).unwrap();
    report.memory_clerks = Some(analysis.report);
    let display = serde_json::to_value(culprit_engine::reports::display(&report)).unwrap();
    assert_eq!(display["memory_clerks"], expected);
    assert_eq!(display["memory_clerks"]["entries"][0]["fields"][1]["value"], "9007199254740993");
}
