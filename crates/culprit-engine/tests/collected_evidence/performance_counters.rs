use super::fixture::Fixture;
use culprit_checks::performance_counters::PerformanceCounterEvidence;
use culprit_collector::analytical::{
    evidence::{EvidenceFamily, EvidenceGapReason},
    test_support::{self, PerformanceCounterCase},
};
use culprit_core::{MeasurementScope, SourceState, telemetry::*};
use culprit_engine::collected_resources;

#[tokio::test]
async fn original_governor_counters_keep_native_group_pool_ids_without_database_ownership_or_cooked_rates() {
    use test_support::{ResourceGovernorCounterCase as Case, resource_governor_counter_evidence};
    for case in Case::ALL {
        let (source, rows) = resource_governor_counter_evidence(case);
        let fixture = Fixture::new(source.clone(), rows.clone()).await;
        let opened = fixture.recovery_object_reads();
        let capture = fixture.read_families(&[EvidenceFamily::OperationalSamples]).await;
        test_support::assert_operational_capture(&capture, &source, &rows);
        let analysis = collected_resources::performance_counters(&capture).unwrap();
        let report = analysis.report;
        assert_eq!(analysis.context.source, source);
        assert_eq!(report.population, culprit_core::PerformanceCounterPopulation::SharedServer);
        assert!(report.database.is_none() && report.native_database_id.is_none());
        let governed = report.entries.iter().filter(|entry| entry.resource_governor.is_some()).collect::<Vec<_>>();
        if matches!(case, Case::Denied | Case::NoRows) {
            assert!(governed.is_empty());
        } else {
            assert_eq!(governed.len(), 7);
            assert!(report.entries[0].resource_governor.is_some());
            for entry in governed {
                let metadata = entry.resource_governor.as_ref().unwrap();
                assert_eq!(entry.scope, MeasurementScope::Server);
                assert_eq!(entry.reset, culprit_core::ResetMarker::Unknown);
                assert_eq!(metadata.native_pool_id.as_deref(), if case == Case::Unmapped { None } else { Some("2") });
                if metadata.kind == MssqlResourceGovernorCounterKind::WorkloadGroup {
                    assert_eq!(
                        metadata.native_group_id.as_deref(),
                        if case == Case::Unmapped { None } else { Some("3") }
                    );
                } else {
                    assert!(metadata.native_group_id.is_none());
                }
                if case == Case::UnknownNativeType {
                    assert_eq!(entry.availability, culprit_core::PerformanceCounterAvailability::UnknownNativeType);
                }
            }
            assert_eq!(report.native_counter_rows, Some(18));
        }
        if matches!(case, Case::Partial | Case::Unmapped | Case::NoRows) {
            assert_eq!(report.source_state, Some(SourceState::Partial));
        }
        assert_eq!(fixture.recovery_object_reads(), opened);
    }
}

#[tokio::test]
async fn missing_governor_body_keeps_original_native_counts_and_never_falls_back_to_current_metadata() {
    let (source, rows) =
        test_support::resource_governor_counter_evidence(test_support::ResourceGovernorCounterCase::Exact);
    let fixture = Fixture::new(source.clone(), rows).await;
    fixture.remove_row(TelemetryStream::OperationalSamples, 2);
    let capture = fixture.read_families(&[EvidenceFamily::OperationalSamples]).await;
    assert!(!capture.is_read_complete());
    let report = collected_resources::performance_counters(&capture).unwrap().report;
    assert_eq!(report.native_observations, Some(3));
    assert_eq!(report.observed_observations, 2);
    assert_eq!(report.native_counter_rows, None);
    assert_eq!(report.observed_counter_rows, 14);
    assert_eq!(report.source_state, Some(SourceState::Partial));
    assert_eq!(report.read_complete, Some(false));
    assert!(report.entries.iter().all(|entry| {
        entry
            .resource_governor
            .as_ref()
            .is_none_or(|context| context.kind != MssqlResourceGovernorCounterKind::WorkloadGroup)
    }));
}

#[tokio::test]
async fn authentic_shared_counter_captures_use_the_pure_report_with_exact_native_rows_and_no_extra_recovery_reads() {
    for case in PerformanceCounterCase::ALL {
        let (source, records) = test_support::performance_counter_evidence(case);
        let fixture = Fixture::new(source.clone(), records.clone()).await;
        let opened = fixture.recovery_object_reads();
        let capture = fixture.read_families(&[EvidenceFamily::OperationalSamples]).await;
        test_support::assert_operational_capture(&capture, &source, &records);
        let analysis = collected_resources::performance_counters(&capture).unwrap();
        let context = NativeReadContext::from_snapshot(&source).unwrap();
        let coverage = &source.observation.snapshot().unwrap().sources[0];
        let rows = records
            .iter()
            .filter_map(|record| {
                let TelemetryRecord::Operational(row) = record else { panic!() };
                (row.source == culprit_core::source_id::PERFORMANCE_COUNTERS).then_some(row)
            })
            .collect::<Vec<_>>();
        let expected = culprit_checks::performance_counters::explain(PerformanceCounterEvidence::Captured {
            context: &context,
            coverage,
            observations: &rows,
            read_complete: true,
        })
        .unwrap();
        assert_eq!(serde_json::to_value(&analysis.report).unwrap(), serde_json::to_value(expected).unwrap());
        assert_eq!(analysis.context.source, source);
        assert_eq!(analysis.context.records.len(), records.len());
        assert_eq!(analysis.report.native_observations, Some(coverage.records));
        assert_eq!(analysis.report.observed_observations, coverage.records);
        assert_eq!(analysis.report.native_source.as_ref(), Some(coverage));
        assert_eq!(analysis.report.read_complete, Some(true));
        assert!(
            analysis
                .report
                .entries
                .iter()
                .all(|entry| entry.scope == MeasurementScope::Server
                    && entry.reset == culprit_core::ResetMarker::Unknown)
        );
        if case == PerformanceCounterCase::Exact {
            assert_eq!(
                analysis
                    .report
                    .entries
                    .iter()
                    .find(|entry| entry.field == "granted_workspace")
                    .unwrap()
                    .metric
                    .text
                    .as_deref(),
                Some("9007199254740993 KiB")
            );
            assert_eq!(
                analysis
                    .report
                    .entries
                    .iter()
                    .find(|entry| entry.field == "grants_pending")
                    .unwrap()
                    .metric
                    .text
                    .as_deref(),
                Some("0")
            );
            assert_eq!(analysis.report.ratios.as_ref().unwrap().declared_ratios, 2);
            assert_eq!(analysis.report.native_counter_rows, Some(11));
        }
        if case == PerformanceCounterCase::MissingFields {
            assert!(
                analysis
                    .report
                    .missing_fields
                    .iter()
                    .any(|field| field.key == "view.performance_counters.field.grants_pending")
            );
            assert!(
                analysis
                    .report
                    .ratios
                    .as_ref()
                    .unwrap()
                    .entries
                    .iter()
                    .any(|entry| entry.issues.contains(&CounterRatioIssue::MissingBase))
            );
        }
        assert_eq!(fixture.recovery_object_reads(), opened);
        assert_eq!(capture.metrics().recovery_objects, 0);
    }
}

#[tokio::test]
async fn a_missing_aggregate_body_retains_original_source_counts_and_unrelated_rows_without_fake_zero() {
    let (source, records) = test_support::performance_counter_evidence(PerformanceCounterCase::Exact);
    let fixture = Fixture::new(source.clone(), records).await;
    fixture.remove_first_row(TelemetryStream::OperationalSamples);
    let opened = fixture.recovery_object_reads();
    let capture = fixture.read_families(&[EvidenceFamily::OperationalSamples]).await;
    assert!(!capture.is_read_complete());
    assert!(capture.gaps().iter().any(|gap| gap.reason == EvidenceGapReason::MissingRows));
    let analysis = collected_resources::performance_counters(&capture).unwrap();
    assert_eq!(analysis.context.records.len(), 1);
    assert_eq!(analysis.context.source, source);
    assert_eq!(analysis.report.native_observations, Some(1));
    assert_eq!(analysis.report.native_counter_rows, None);
    assert_eq!(analysis.report.observed_counter_rows, 0);
    assert_eq!(analysis.report.source_state, Some(SourceState::Partial));
    assert!(analysis.report.entries.is_empty());
    assert_eq!(fixture.recovery_object_reads(), opened);
}

#[tokio::test]
async fn authenticated_counter_sets_with_contradictory_native_semantics_are_refused_not_repaired() {
    for cause in ["unit", "scope", "floating", "duplicate-native"] {
        let (source, mut records) = test_support::performance_counter_evidence(PerformanceCounterCase::Exact);
        let TelemetryRecord::Operational(OperationalObservation { fact: OperationalFact::CounterSet(set), .. }) =
            &mut records[0]
        else {
            panic!()
        };
        match cause {
            "unit" => set.counters[0].unit = CounterUnit::Bytes,
            "scope" => {
                set.scope = MeasurementScope::Database;
                for row in &mut set.counters {
                    row.scope = MeasurementScope::Database;
                }
            }
            "floating" => set.counters[0].value = MeasurementValue::Real(1.0),
            _ => {
                let mut row = set.counters[0].clone();
                row.key = "another-key".into();
                set.counters.push(row);
                set.properties.insert(PERFORMANCE_COUNTER_ROWS_PROPERTY.into(), "12".into());
            }
        }
        assert!(set.is_valid());
        let fixture = Fixture::new(source, records).await;
        let opened = fixture.recovery_object_reads();
        let capture = fixture.read_families(&[EvidenceFamily::OperationalSamples]).await;
        assert!(collected_resources::performance_counters(&capture).is_err(), "{cause}");
        assert_eq!(fixture.recovery_object_reads(), opened);
    }
}

#[tokio::test]
async fn public_report_display_keeps_a_counter_collection_and_present_exact_text() {
    let snapshot = serde_json::from_str(include_str!("../../../../fixtures/postgres-demo.json")).unwrap();
    let mut report = culprit_checks::analyze(snapshot);
    assert_eq!(
        serde_json::to_value(culprit_engine::reports::display(&report)).unwrap()["performance_counters"],
        serde_json::json!([])
    );
    let (source, records) = test_support::performance_counter_evidence(PerformanceCounterCase::Exact);
    let fixture = Fixture::new(source, records).await;
    let capture = fixture.read_families(&[EvidenceFamily::OperationalSamples]).await;
    report.performance_counters = vec![*collected_resources::performance_counters(&capture).unwrap().report];
    let display = serde_json::to_value(culprit_engine::reports::display(&report)).unwrap();
    assert_eq!(display["performance_counters"][0]["entries"][0]["raw_value"], "9007199254740993");
}

#[tokio::test]
async fn selected_database_counter_captures_preserve_original_identity_partial_scope_and_unavailable_without_hydration()
{
    use test_support::{DatabasePerformanceCounterCase as Case, database_performance_counter_evidence};
    for case in Case::ALL {
        let (source, records) = database_performance_counter_evidence(case);
        let fixture = Fixture::new(source.clone(), records.clone()).await;
        let opened = fixture.recovery_object_reads();
        let capture = fixture.read_families(&[EvidenceFamily::OperationalSamples]).await;
        test_support::assert_operational_capture(&capture, &source, &records);
        let analysis = collected_resources::performance_counters(&capture).unwrap();
        assert_eq!(analysis.context.source, source);
        assert_eq!(analysis.report.database.as_deref(), Some("shop"));
        assert_eq!(analysis.report.native_database_id.as_deref(), Some("7"));
        assert_eq!(analysis.report.read_complete, Some(true));
        assert!(analysis.report.ratios.is_none());
        assert!(analysis.report.entries.iter().all(|entry| entry.scope == MeasurementScope::Database
            && entry.native_instance.as_deref() == Some("shop")
            && entry.reset == culprit_core::ResetMarker::Unknown));
        match case {
            Case::Denied => {
                assert_eq!(analysis.report.source_state, Some(SourceState::Unavailable));
                assert_eq!(analysis.report.native_counter_rows, None);
                assert!(analysis.report.entries.is_empty());
            }
            Case::Partial => {
                assert_eq!(analysis.report.source_state, Some(SourceState::Partial));
                assert_eq!(analysis.report.entries.len(), 2);
                assert_eq!(analysis.report.missing_fields.len(), 1);
            }
            Case::Exact => {
                assert_eq!(analysis.report.native_counter_rows, Some(3));
                assert_eq!(analysis.report.entries.len(), 3);
                assert!(analysis.report.missing_fields.is_empty());
                assert_eq!(analysis.report.entries[0].raw_value, "9007199254740993");
            }
            Case::UnknownNativeType => {
                assert_eq!(
                    analysis.report.entries[0].availability,
                    culprit_core::PerformanceCounterAvailability::UnknownNativeType
                );
                assert_eq!(analysis.report.entries[0].unit, CounterUnit::Unknown);
            }
        }
        assert_eq!(fixture.recovery_object_reads(), opened);
        assert_eq!(capture.metrics().recovery_objects, 0);
    }
}
#[tokio::test]
async fn a_missing_selected_database_counter_body_keeps_authenticated_database_without_zero_or_shared_total() {
    let (source, records) =
        test_support::database_performance_counter_evidence(test_support::DatabasePerformanceCounterCase::Exact);
    let fixture = Fixture::new(source.clone(), records).await;
    fixture.remove_first_row(TelemetryStream::OperationalSamples);
    let capture = fixture.read_families(&[EvidenceFamily::OperationalSamples]).await;
    assert!(!capture.is_read_complete());
    let analysis = collected_resources::performance_counters(&capture).unwrap();
    assert_eq!(analysis.context.source, source);
    assert_eq!(analysis.report.database.as_deref(), Some("shop"));
    assert_eq!(analysis.report.native_observations, Some(1));
    assert_eq!(analysis.report.native_counter_rows, None);
    assert_eq!(analysis.report.source_state, Some(SourceState::Partial));
    assert!(analysis.report.entries.is_empty());
}

#[tokio::test]
async fn original_common_counter_inputs_share_canonical_semantics_without_rates_health_or_hydration() {
    use test_support::{CommonPerformanceCounterCase as Case, common_performance_counter_evidence};
    for case in Case::ALL {
        let (source, rows) = common_performance_counter_evidence(case);
        let fixture = Fixture::new(source.clone(), rows.clone()).await;
        let opened = fixture.recovery_object_reads();
        let capture = fixture.read_families(&[EvidenceFamily::OperationalSamples]).await;
        test_support::assert_common_performance_counter_capture(&capture, &source, &rows, case);
        let analysis = collected_resources::performance_counters(&capture).unwrap();
        let report = &analysis.report;
        assert_eq!(analysis.context.source, source);
        assert_eq!(report.population, culprit_core::PerformanceCounterPopulation::SharedServer);
        assert!(report.database.is_none() && report.native_database_id.is_none());
        assert_eq!(report.read_complete, Some(true));
        assert_eq!(report.source_state, Some(source.observation.snapshot().unwrap().sources[0].state));
        let detailed = report
            .entries
            .iter()
            .filter(|entry| entry.family == culprit_core::telemetry::PerformanceCounterFamily::AccessMethods)
            .collect::<Vec<_>>();
        assert_eq!(detailed.len(), if matches!(case, Case::Denied | Case::MissingCommon) { 0 } else { 6 });
        for entry in detailed {
            assert_eq!(entry.native_object, "SQLServer:Access Methods");
            assert!(entry.native_instance.is_none());
            assert_eq!(entry.scope, culprit_core::MeasurementScope::Server);
            assert_eq!(entry.reset, culprit_core::ResetMarker::Unknown);
            assert_eq!(entry.raw_value, if case == Case::MeasuredZero { "0" } else { "9007199254740993" });
            let unknown_type = case == Case::UnknownNativeType && entry.field == "detailed_forwarded_records";
            assert_eq!(
                entry.explanation,
                if unknown_type {
                    culprit_core::t!("view.performance_counters.limit.native_type")
                } else {
                    culprit_core::t!("view.performance_counters.explain.access_methods")
                }
            );
            assert_eq!(
                entry.availability == culprit_core::PerformanceCounterAvailability::UnknownNativeType,
                unknown_type
            );
        }
        if case == Case::MissingCommon {
            assert!(
                report
                    .missing_fields
                    .iter()
                    .any(|field| field.key == "view.performance_counters.field.detailed_full_scans")
            );
        }
        if case == Case::Denied {
            assert!(report.entries.is_empty() && report.ratios.is_none());
            assert_eq!(report.native_counter_rows, None);
        } else {
            let average = report
                .ratios
                .as_ref()
                .unwrap()
                .entries
                .iter()
                .find(|ratio| ratio.kind == CounterRatioKind::LockWaitAverageInputs)
                .unwrap();
            assert_eq!(average.metrics.len(), 2);
            assert!(average.metrics.iter().all(|metric| metric.key != "view.counter_ratios.metric.percent"));
            assert!(average.issues.contains(&CounterRatioIssue::IntervalRequired));
            assert!(!average.issues.contains(&CounterRatioIssue::OutsideSubset));
            assert_eq!(average.native_object.is_some(), case != Case::MissingCommon);
            if !matches!(case, Case::MissingCommon | Case::MissingBase) {
                assert_eq!(
                    average.metrics[0].text.as_deref(),
                    Some(if case == Case::MeasuredZero { "0" } else { "4500" })
                );
                assert_eq!(
                    average.metrics[1].text.as_deref(),
                    Some(if case == Case::MeasuredZero { "0" } else { "3" })
                );
            }
            let fraction = report
                .ratios
                .as_ref()
                .unwrap()
                .entries
                .iter()
                .find(|ratio| ratio.kind == CounterRatioKind::BufferCacheHit)
                .unwrap();
            assert!(
                fraction
                    .explanation
                    .contains(&culprit_core::t!("view.performance_counters.buffer_cache_ratio.explanation"))
            );
            let percent = fraction.metrics.iter().find(|metric| metric.key == "view.counter_ratios.metric.percent");
            assert_eq!(
                percent.is_some(),
                !matches!(case, Case::MeasuredZero | Case::MissingBase | Case::MissingCommon)
            );
            if case == Case::MissingCommon {
                assert!(fraction.native_object.is_none());
                assert!(!report.entries.iter().any(|entry| entry.field.starts_with("common_")));
                assert!(
                    report
                        .missing_fields
                        .iter()
                        .any(|field| field.key == "view.performance_counters.field.common_tempdb_free")
                );
            } else {
                let free = report.entries.iter().find(|entry| entry.field == "common_tempdb_free").unwrap();
                assert_eq!(free.native_object, "SQLServer:Transactions");
                assert_eq!(free.native_instance, None);
                assert_eq!(free.raw_value, if case == Case::MeasuredZero { "0" } else { "9007199254740993" });
                assert_eq!(free.explanation, culprit_core::t!("view.performance_counters.explain.common_tempdb"));
                let page = report.entries.iter().find(|entry| entry.field == "common_page_reads").unwrap();
                assert_eq!(
                    page.availability == culprit_core::PerformanceCounterAvailability::UnknownNativeType,
                    case == Case::UnknownNativeType
                );
                if case != Case::UnknownNativeType {
                    assert_eq!(page.nature, CounterNature::Cumulative);
                    assert_eq!(page.unit, CounterUnit::Pages { bytes_per_page: 8192 });
                }
            }
        }
        assert_eq!(capture.metrics().recovery_objects, 0);
        assert_eq!(fixture.recovery_object_reads(), opened);
    }
}

#[tokio::test]
async fn a_missing_common_original_body_withholds_its_fraction_but_keeps_original_counts_and_other_sources() {
    let (source, rows) =
        test_support::common_performance_counter_evidence(test_support::CommonPerformanceCounterCase::Observed);
    let fixture = Fixture::new(source.clone(), rows).await;
    fixture.remove_first_row(TelemetryStream::OperationalSamples);
    let opened = fixture.recovery_object_reads();
    let capture = fixture.read_families(&[EvidenceFamily::OperationalSamples]).await;
    let report = collected_resources::performance_counters(&capture).unwrap().report;
    assert_eq!(report.native_observations, Some(1));
    assert_eq!(report.observed_observations, 0);
    assert_eq!(report.native_counter_rows, None);
    assert_eq!(report.source_state, Some(SourceState::Partial));
    assert_eq!(report.read_complete, Some(false));
    assert!(report.ratios.is_none() && report.entries.is_empty());
    assert_eq!(capture.records().len(), 1, "independent original source remains");
    assert_eq!(fixture.recovery_object_reads(), opened);
}
