use super::*;
use culprit_collector::analytical::test_support::{query_population_evidence, query_population_evidence_cases};
use culprit_core::collected_analysis::{
    CollectedQueryAnalysis, CollectedQueryAnalysisResult, CollectedQueryPopulation,
};

fn identities(source: &CollectionDescription) -> Vec<NativeIdentity> {
    source
        .query_history()
        .unwrap()
        .population_catalog
        .as_ref()
        .unwrap()
        .populations
        .iter()
        .map(|population| population.identity.clone())
        .collect()
}

fn result<'a>(analysis: &'a CollectedQueryAnalysis, identity: &NativeIdentity) -> &'a CollectedQueryAnalysisResult {
    analysis
        .analyses
        .iter()
        .find(|result| {
            matches!(&result.population,
        CollectedQueryPopulation::Population {identity: actual} if actual == identity)
        })
        .unwrap()
}

fn has_limit(result: &CollectedQueryAnalysisResult, reason: CollectedAnalysisLimit) -> bool {
    result.limitations.iter().any(|limit| limit.reason == reason)
}

#[tokio::test]
async fn all_native_populations_get_independent_denominators_queries_plans_and_waits_from_one_context() {
    let (source, records) = query_population_evidence();
    let populations = identities(&source);
    let fixture = Fixture::new(source.clone(), records.clone()).await;
    let capture = fixture.read().await;
    let analysis = collected_evidence::analyze(&capture, None).unwrap();
    assert_eq!(analysis.requested_population, None);
    assert_eq!(analysis.analyses.len(), 2);
    assert_eq!(analysis.context.source, source);
    assert_eq!(analysis.context.records.len(), records.len());
    let west = result(&analysis, &populations[0]);
    let east = result(&analysis, &populations[1]);
    let (west_workload, east_workload) = (west.workload.as_ref().unwrap(), east.workload.as_ref().unwrap());
    assert_eq!(
        (west_workload.total_executions, west_workload.total_time_ms, west_workload.total_cpu_ms),
        (2.0, 20.0, Some(10.0))
    );
    assert_eq!(
        (east_workload.total_executions, east_workload.total_time_ms, east_workload.total_cpu_ms),
        (5.0, 500.0, Some(250.0))
    );
    let west_query = &west_workload.queries[0];
    let east_query = &east_workload.queries[0];
    assert_ne!(west_query.id, east_query.id, "the same opaque cohort cannot erase the declared population");
    assert_eq!((west_query.mean_time_ms, east_query.mean_time_ms), (10.0, 100.0));
    assert_eq!((west_query.plans.len(), east_query.plans.len()), (1, 1));
    assert_eq!(west_query.plans[0].id, east_query.plans[0].id, "the native definition remains shared logical identity");
    assert_eq!((west_query.plans[0].executions, east_query.plans[0].executions), (2.0, 5.0));
    assert_eq!(west_query.observed_wait_ms(WaitClass::Lock), Some(7.0));
    assert_eq!(east_query.observed_wait_ms(WaitClass::Io), Some(90.0));
    assert!(!west_query.waits.iter().any(|wait| wait.category == WaitClass::Io.category()));
    assert!(!east_query.waits.iter().any(|wait| wait.category == WaitClass::Lock.category()));
    for section in &analysis.analyses {
        let CollectedQueryPopulation::Population { identity } = &section.population else { panic!() };
        assert_eq!(section.workload.as_ref().unwrap().population.as_ref(), Some(identity));
        assert!(
            section.workload.as_ref().unwrap().population_catalog.is_none(),
            "catalog and proof belong to one context"
        );
        assert!(section.findings.iter().all(|finding| finding.check != "parameter-sensitive"));
        assert_eq!(section.query_inputs.len(), 1);
        assert_eq!(section.query_inputs[0].measurements.len(), 1);
        assert!(section.query_inputs[0].measurements.iter().all(|key| matches!(
            source.query_history().unwrap().execution_population(&key.native.qualifiers).unwrap(),
            QueryExecutionPopulation::Population(actual) if actual == identity)));
    }
    let serialized = serde_json::to_value(&analysis).unwrap();
    assert!(serialized.get("workload").is_none());
    assert!(serialized["analyses"].as_array().unwrap().iter().all(|result| result.get("context").is_none()));
    assert_eq!(analysis.context.metrics.queries, capture.metrics().queries, "pure analysis does not reread any source");
}

#[tokio::test]
async fn an_explicit_full_native_identity_selects_the_same_view_without_changing_original_evidence() {
    let (source, records) = query_population_evidence();
    let fixture = Fixture::new(source.clone(), records).await;
    let capture = fixture.read().await;
    let combined = collected_evidence::analyze(&capture, None).unwrap();
    for population in identities(&source) {
        let selected = collected_evidence::analyze(&capture, Some(&population)).unwrap();
        assert_eq!(selected.requested_population, Some(population.clone()));
        assert_eq!(selected.analyses.len(), 1);
        assert_eq!(
            serde_json::to_value(&selected.analyses[0].workload).unwrap(),
            serde_json::to_value(&result(&combined, &population).workload).unwrap()
        );
        assert_eq!(
            serde_json::to_value(&selected.analyses[0].query_inputs).unwrap(),
            serde_json::to_value(&result(&combined, &population).query_inputs).unwrap()
        );
        assert_eq!(serde_json::to_value(&selected.context).unwrap(), serde_json::to_value(&combined.context).unwrap());
        let mut wrong_generation = population.clone();
        wrong_generation.qualifiers.insert("native-generation".into(), "different".into());
        assert_eq!(
            collected_evidence::analyze(&capture, Some(&wrong_generation)).unwrap_err().code,
            culprit_core::ErrorCode::Unsupported
        );
        assert_eq!(
            collected_evidence::analyze(&capture, Some(&NativeIdentity::new(&population.id))).unwrap_err().code,
            culprit_core::ErrorCode::Unsupported
        );
    }
}

#[tokio::test]
async fn source_declared_empty_unknown_and_unpartitioned_populations_do_not_substitute_each_other() {
    for (index, (source, records)) in query_population_evidence_cases().into_iter().enumerate().skip(1) {
        let fixture = Fixture::new(source.clone(), records.clone()).await;
        let capture = fixture.read().await;
        let analysis = collected_evidence::analyze(&capture, None).unwrap();
        assert_eq!(analysis.context.records.len(), records.len());
        match index {
            1 => {
                let populations = identities(&source);
                assert_eq!(analysis.analyses.len(), 2);
                assert_eq!(result(&analysis, &populations[0]).workload.as_ref().unwrap().total_time_ms, 20.0);
                let empty = result(&analysis, &populations[1]);
                assert!(empty.workload.is_none() && empty.query_inputs.is_empty());
                assert!(!has_limit(empty, CollectedAnalysisLimit::ExecutionPopulationUnknown));
                let selected = collected_evidence::analyze(&capture, Some(&populations[1])).unwrap();
                assert_eq!(selected.analyses[0].population, empty.population);
                assert!(selected.analyses[0].workload.is_none());
            }
            2 | 3 => {
                assert_eq!(analysis.analyses.len(), 1);
                let unknown = &analysis.analyses[0];
                assert_eq!(unknown.population, CollectedQueryPopulation::Unknown);
                assert!(unknown.workload.is_none() && unknown.query_inputs.is_empty());
                assert!(has_limit(unknown, CollectedAnalysisLimit::ExecutionPopulationUnknown));
                assert_eq!(
                    collected_evidence::analyze(
                        &capture,
                        Some(&NativeIdentity::new("history-group-z").qualified("native-generation", "7"))
                    )
                    .unwrap_err()
                    .code,
                    culprit_core::ErrorCode::Unsupported
                );
            }
            4 => {
                assert_eq!(analysis.analyses.len(), 1);
                assert_eq!(analysis.analyses[0].population, CollectedQueryPopulation::Unpartitioned);
                let workload = analysis.analyses[0].workload.as_ref().unwrap();
                assert_eq!((workload.total_executions, workload.total_time_ms), (2.0, 20.0));
                assert!(workload.population.is_none());
                assert!(collected_evidence::analyze(&capture, Some(&NativeIdentity::new("anything"))).is_err());
            }
            _ => panic!("unreviewed shared population case"),
        }
    }
}

#[tokio::test]
async fn missing_definitions_in_another_population_do_not_change_the_selected_denominator() {
    let (source, mut records) = query_population_evidence();
    let populations = identities(&source);
    let mut missing = records
        .iter()
        .rev()
        .find_map(|record| match record {
            TelemetryRecord::QueryRuntime(row) => Some(row.clone()),
            _ => None,
        })
        .unwrap();
    let identity = NativeQueryIdentity { query_id: "missing-native-query".into(), qualifiers: Default::default() };
    missing.key.native.id = "later-missing-query-runtime".into();
    missing.key.references = StatementReferences {
        query: Some(NativeDefinitionReference {
            native: identity.native(),
            definition_id: Some(identity.definition_id(&source).unwrap()),
        }),
        plan: None,
    };
    missing.executions = "100".into();
    records.push(TelemetryRecord::QueryRuntime(missing));
    let fixture = Fixture::new(source, records).await;
    let capture = fixture.read().await;
    let analysis = collected_evidence::analyze(&capture, None).unwrap();
    let west = result(&analysis, &populations[0]);
    let east = result(&analysis, &populations[1]);
    assert_eq!(west.workload.as_ref().unwrap().total_time_ms, 20.0);
    assert!(!has_limit(west, CollectedAnalysisLimit::MissingQueryDefinition));
    let workload = east.workload.as_ref().unwrap();
    assert_eq!((workload.total_executions, workload.total_time_ms), (105.0, 10_500.0));
    assert_eq!(workload.queries[0].total_time_ms, 500.0);
    assert!(has_limit(east, CollectedAnalysisLimit::MissingQueryDefinition));
}

#[tokio::test]
async fn a_runtime_gap_never_becomes_a_smaller_selected_population_denominator() {
    let (source, records) = query_population_evidence();
    let populations = identities(&source);
    let fixture = Fixture::new(source, records).await;
    fixture.remove_first_runtime();
    let capture = fixture.read().await;
    assert!(!capture.gaps().is_empty());
    let selected = collected_evidence::analyze(&capture, Some(&populations[1])).unwrap();
    let result = &selected.analyses[0];
    assert!(result.workload.is_none());
    assert!(has_limit(result, CollectedAnalysisLimit::RuntimeDenominatorUnavailable));
    assert_eq!(
        result.query_inputs.len(),
        1,
        "positive references survive but missing native membership cannot establish totals"
    );
    assert_eq!(selected.context.read_gaps.len(), capture.gaps().len());
}

#[tokio::test]
async fn an_incomplete_wait_family_preserves_selected_positives_without_borrowed_zeros() {
    let (source, records) = query_population_evidence();
    let populations = identities(&source);
    let fixture = Fixture::new(source, records).await;
    fixture.remove_first_row(TelemetryStream::QueryWaits);
    let capture = fixture.read().await;
    let selected = collected_evidence::analyze(&capture, Some(&populations[1])).unwrap();
    let result = &selected.analyses[0];
    let query = &result.workload.as_ref().unwrap().queries[0];
    assert_eq!(query.observed_wait_ms(WaitClass::Io), Some(90.0));
    assert!(!query.waits_coverage.as_ref().unwrap().complete);
    assert_eq!(query.observed_wait_ms(WaitClass::Lock), None);
    assert!(has_limit(result, CollectedAnalysisLimit::WaitFamilyIncomplete));
}

#[tokio::test]
async fn source_cohort_outcomes_stay_separate_inside_a_selected_population() {
    let (source, mut records) = query_population_evidence();
    let population = identities(&source)[0].clone();
    let mut failed = records
        .iter()
        .find_map(|record| match record {
            TelemetryRecord::QueryRuntime(row) => Some(row.clone()),
            _ => None,
        })
        .unwrap();
    failed.key.native.id = "native-failed-fragment".into();
    failed.key.native.qualifiers.insert("execution_type".into(), "4".into());
    failed.key.cohort.as_mut().unwrap().qualifiers.insert("native-outcome".into(), "failed".into());
    failed.executions = "3".into();
    failed.metrics[0].average = 50.0;
    failed.metrics[0].maximum = Some(50.0);
    records.push(TelemetryRecord::QueryRuntime(failed));
    let fixture = Fixture::new(source, records).await;
    let capture = fixture.read().await;
    let selected = collected_evidence::analyze(&capture, Some(&population)).unwrap();
    let result = &selected.analyses[0];
    let workload = result.workload.as_ref().unwrap();
    assert_eq!((workload.total_executions, workload.total_time_ms), (5.0, 170.0));
    assert_eq!(workload.queries.len(), 2);
    let completed = workload.queries.iter().find(|query| query.executions == 2.0).unwrap();
    let failed = workload.queries.iter().find(|query| query.executions == 3.0).unwrap();
    assert_eq!((completed.mean_time_ms, failed.mean_time_ms), (10.0, 50.0));
    assert_eq!(completed.observed_wait_ms(WaitClass::Lock), Some(7.0));
    assert!(failed.waits.is_empty());
    assert!(result.findings.iter().all(|finding| finding.check != "parameter-sensitive"));
}

#[tokio::test]
async fn precision_failure_in_another_group_cannot_erase_valid_selected_counts() {
    let (source, mut records) = query_population_evidence();
    let populations = identities(&source);
    let TelemetryRecord::QueryRuntime(runtime) =
        records.iter_mut().rev().find(|record| matches!(record, TelemetryRecord::QueryRuntime(_))).unwrap()
    else {
        panic!()
    };
    runtime.executions = "9007199254740993".into();
    let fixture = Fixture::new(source, records).await;
    let capture = fixture.read().await;
    let analysis = collected_evidence::analyze(&capture, None).unwrap();
    let west = result(&analysis, &populations[0]);
    let east = result(&analysis, &populations[1]);
    assert_eq!(
        (west.workload.as_ref().unwrap().total_executions, west.workload.as_ref().unwrap().total_time_ms),
        (2.0, 20.0)
    );
    assert!(!has_limit(west, CollectedAnalysisLimit::CounterPrecision));
    assert!(east.workload.is_none());
    assert!(has_limit(east, CollectedAnalysisLimit::CounterPrecision));
}

#[tokio::test]
async fn a_declared_empty_partitioned_catalog_keeps_one_explicit_unavailable_section() {
    let (mut source, mut records) = query_population_evidence();
    let SourceObservation::QueryHistory(history) = &mut source.observation else { panic!() };
    history.population_binding = QueryHistoryPopulationBinding::Dimensions { bindings: vec![] };
    let catalog = history.population_catalog.as_mut().unwrap();
    catalog.populations.clear();
    catalog.requires_selection = false;
    assert!(history.is_valid());
    records.retain(|record| record.measurement_key().is_none());
    let fixture = Fixture::new(source.clone(), records).await;
    let capture = fixture.read().await;
    assert!(capture.is_read_complete());
    let opens = fixture.recovery_object_reads();
    let analysis = collected_evidence::analyze(&capture, None).unwrap();
    assert_eq!(analysis.context.source, source, "the frozen empty catalog is not replaced by an invented population");
    assert_eq!(analysis.context.records.len(), 2);
    assert_eq!(analysis.requested_population, None);
    assert_eq!(analysis.analyses.len(), 1);
    let unknown = &analysis.analyses[0];
    assert_eq!(unknown.population, CollectedQueryPopulation::Unknown);
    assert!(unknown.workload.is_none() && unknown.query_inputs.is_empty());
    assert!(has_limit(unknown, CollectedAnalysisLimit::ExecutionPopulationUnknown));
    assert_eq!(
        collected_evidence::analyze(&capture, Some(&NativeIdentity::new("any-native-id"))).unwrap_err().code,
        culprit_core::ErrorCode::Unsupported
    );
    assert_eq!(fixture.recovery_object_reads(), opens, "pure analysis opens no native or recovery payload source");
}

#[tokio::test]
async fn the_maximum_declared_population_set_assigns_every_measurement_to_one_independent_view() {
    let (mut source, mut records) = query_population_evidence();
    let runtime = records
        .iter()
        .find_map(|record| match record {
            TelemetryRecord::QueryRuntime(row) => Some(row.clone()),
            _ => None,
        })
        .unwrap();
    let wait = records
        .iter()
        .find_map(|record| match record {
            TelemetryRecord::QueryWaits(row) => Some(row.clone()),
            _ => None,
        })
        .unwrap();
    records.retain(|record| record.measurement_key().is_none());
    let history = match &mut source.observation {
        SourceObservation::QueryHistory(history) => history,
        _ => panic!(),
    };
    let QueryHistoryPopulationBinding::Dimensions { bindings } = &history.population_binding else { panic!() };
    let original_dimensions = bindings[0].qualifiers.keys().cloned().collect::<Vec<_>>();
    for dimension in &original_dimensions {
        history.dimensions.remove(dimension);
    }
    history.dimensions.insert("source-part".into(), (0..8).map(|part| format!("part-{part}")).collect());
    history.dimensions.insert("source-slice".into(), (0..16).map(|slice| format!("slice-{slice}")).collect());
    let mut populations = Vec::new();
    let mut bindings = Vec::new();
    for index in 0..128u64 {
        let identity = NativeIdentity::new(format!("native-population-{index:03}")).qualified("epoch", "stable");
        let qualifiers = [
            ("source-part".into(), format!("part-{}", index / 16)),
            ("source-slice".into(), format!("slice-{}", index % 16)),
        ]
        .into_iter()
        .collect::<std::collections::BTreeMap<_, _>>();
        populations.push(culprit_core::NativeRuntimePopulation {
            identity: identity.clone(),
            native_properties: Default::default(),
        });
        bindings.push(QueryHistoryPopulationBindingEntry { population: identity, qualifiers: qualifiers.clone() });
        let mut runtime = runtime.clone();
        let mut wait = wait.clone();
        for key in [&mut runtime.key, &mut wait.key] {
            for dimension in &original_dimensions {
                key.native.qualifiers.remove(dimension);
            }
            key.native.qualifiers.extend(qualifiers.clone());
        }
        runtime.executions = (index + 1).to_string();
        runtime.metrics[0].average = 1.0;
        runtime.metrics[0].minimum = Some(0.5);
        runtime.metrics[0].maximum = Some(2.0);
        wait.total = MeasurementValue::Integer((index + 1).to_string());
        records.push(TelemetryRecord::QueryRuntime(runtime));
        records.push(TelemetryRecord::QueryWaits(wait));
    }
    history.population_binding = QueryHistoryPopulationBinding::Dimensions { bindings };
    history.population_catalog.as_mut().unwrap().populations = populations;
    assert!(history.is_valid());
    let fixture = Fixture::new(source, records).await;
    let capture = fixture.read().await;
    let analysis = collected_evidence::analyze(&capture, None).unwrap();
    assert_eq!(analysis.analyses.len(), 128);
    assert_eq!(analysis.context.records.len(), 258);
    let mut query_ids = std::collections::BTreeSet::new();
    let mut measurements = 0;
    let mut executions = 0.0;
    let mut wait_ms = 0.0;
    for (index, result) in analysis.analyses.iter().enumerate() {
        let workload = result.workload.as_ref().unwrap();
        assert_eq!((workload.total_executions, workload.total_time_ms), ((index + 1) as f64, (index + 1) as f64));
        assert_eq!(workload.queries.len(), 1);
        assert!(query_ids.insert(workload.queries[0].id.clone()));
        measurements += result.query_inputs.iter().map(|input| input.measurements.len()).sum::<usize>();
        executions += workload.total_executions;
        wait_ms += workload.queries[0].observed_wait_ms(WaitClass::Lock).unwrap();
    }
    assert_eq!(measurements, 128, "each original runtime row belongs to exactly one view");
    assert_eq!(executions, 8256.0);
    assert_eq!(wait_ms, 8256.0, "wait observations are assigned once without multiplication by group count");
}
