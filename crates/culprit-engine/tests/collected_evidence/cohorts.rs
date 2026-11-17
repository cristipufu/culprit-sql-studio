use super::*;
use culprit_collector::analytical::test_support::{
    QueryCohortCase, assert_query_cohort_capture, query_cohort_evidence,
};
use culprit_core::{Actionability, FindingRuntimeVariation, collected_analysis::CollectedQueryAnalysis};

async fn analyze_case(case: QueryCohortCase) -> CollectedQueryAnalysis {
    let (source, records) = query_cohort_evidence(case);
    let fixture = Fixture::new(source.clone(), records.clone()).await;
    let capture = fixture.read().await;
    assert_query_cohort_capture(&capture, &source, &records);
    let analysis = collected_evidence::analyze(&capture, None).unwrap();
    assert_eq!(analysis.context.records.len(), records.len());
    for input in &analysis.analyses[0].query_inputs {
        for measurement in &input.measurements {
            assert!(
                records.iter().any(|row| matches!(row, TelemetryRecord::QueryRuntime(row) if &row.key == measurement))
            );
        }
    }
    analysis
}

fn plan_id<'a>(analysis: &'a CollectedQueryAnalysis, native_id: &str) -> &'a str {
    analysis
        .context
        .records
        .iter()
        .find_map(|row| match &row.row.record {
            TelemetryRecord::PlanDefinition(plan) if plan.identity.plan_id == native_id => Some(plan.id.as_str()),
            _ => None,
        })
        .unwrap()
}

#[tokio::test]
async fn a_qualified_population_reaches_the_existing_variation_check_with_exact_weighted_plan_costs() {
    let analysis = analyze_case(QueryCohortCase::PlanVariation).await;
    let workload = analysis.analyses[0].workload.as_ref().unwrap();
    assert_eq!(workload.queries.len(), 1);
    assert_eq!(workload.query_count, 1.0);
    assert_eq!(workload.total_executions, 50.0);
    assert_eq!(workload.total_time_ms, 9500.0);
    let query = &workload.queries[0];
    assert_eq!(query.executions, 50.0);
    assert_eq!(query.total_time_ms, 9500.0);
    assert_eq!(query.mean_time_ms, 190.0);
    assert_eq!(query.cpu_time_ms, Some(4750.0));
    assert_eq!(query.plans.len(), 2);
    let slow = query.plans.iter().find(|plan| plan.id == plan_id(&analysis, "47")).unwrap();
    assert_eq!((slow.executions, slow.total_time_ms, slow.mean_time_ms), (10.0, 9100.0, 910.0));
    assert_eq!(slow.stddev_time_ms, None);
    let fast = query.plans.iter().find(|plan| plan.id == plan_id(&analysis, "8")).unwrap();
    assert_eq!((fast.executions, fast.total_time_ms, fast.mean_time_ms), (40.0, 400.0, 10.0));
    assert!(query.plans.iter().all(|plan| plan.is_forced.is_none() && plan.compiled.is_none()));
    assert_eq!(query.observed_wait_ms(WaitClass::Lock), Some(30.0));
    let input = &analysis.analyses[0].query_inputs[0];
    assert_eq!(input.measurements.len(), 3);
    assert!(input.references.query.is_some() && input.references.plan.is_none());
    assert!(input.measurements.iter().all(|key| key.cohort.is_some() && key.native.qualifiers.contains_key("plan_id")));
    let finding = analysis.analyses[0].findings.iter().find(|finding| finding.check == "parameter-sensitive").unwrap();
    assert_eq!(finding.actionability, Actionability::Information);
    assert!(
        matches!(&finding.facts.runtime_variation, Some(FindingRuntimeVariation::PlanAverages { faster_plan, slower_plan })
        if faster_plan == &fast.id && slower_plan == &slow.id)
    );
    assert!(finding.fix.as_ref().unwrap().sql.is_none());
    assert!(!analysis.analyses[0].findings.iter().any(|finding| finding.check == "plan-regression"));
}

#[tokio::test]
async fn converged_plan_means_and_native_psp_routing_do_not_establish_a_parameter_cause() {
    for case in [QueryCohortCase::ConvergedPlans, QueryCohortCase::HealthyVariantFamily] {
        let analysis = analyze_case(case).await;
        let workload = analysis.analyses[0].workload.as_ref().unwrap();
        assert_eq!((workload.total_executions, workload.total_time_ms), (50.0, 500.0));
        assert_eq!(workload.queries.len(), 1);
        let query = &workload.queries[0];
        assert_eq!(query.plans.len(), 2);
        assert!(query.plans.iter().all(|plan| plan.mean_time_ms == 10.0));
        assert_eq!(query.variant_family.is_some(), case == QueryCohortCase::HealthyVariantFamily);
        assert!(!analysis.analyses[0].findings.iter().any(|finding| finding.check == "parameter-sensitive"));
        if let Some(family) = &query.variant_family {
            assert!(analysis.context.records.iter().all(|row| !matches!(&row.row.record,
                TelemetryRecord::QueryDefinition(query) if query.identity == family.parent)));
            assert!(analysis.context.records.iter().all(|row| !matches!(&row.row.record,
                TelemetryRecord::PlanDefinition(plan) if plan.identity == family.dispatcher)));
        }
    }
}

#[tokio::test]
async fn unknown_populations_and_distinct_context_outcome_replica_or_future_dimensions_cannot_create_plan_variation() {
    for case in [
        QueryCohortCase::UnknownCohort,
        QueryCohortCase::OtherContext,
        QueryCohortCase::OtherObject,
        QueryCohortCase::OtherLifetime,
        QueryCohortCase::OtherOutcome,
        QueryCohortCase::FutureOutcome,
        QueryCohortCase::OtherReplica,
        QueryCohortCase::OtherNativeDimension,
    ] {
        let analysis = analyze_case(case).await;
        let workload = analysis.analyses[0].workload.as_ref().unwrap();
        assert_eq!((workload.total_executions, workload.total_time_ms), (50.0, 9500.0), "{case:?}");
        assert_eq!(workload.queries.len(), 2, "{case:?}");
        assert!(workload.queries.iter().all(|query| query.plans.len() == 1));
        for (executions, lock_ms) in [(10.0, 25.0), (40.0, 5.0)] {
            let query = workload.queries.iter().find(|query| query.executions == executions).unwrap();
            assert_eq!(query.observed_wait_ms(WaitClass::Lock), Some(lock_ms), "{case:?}");
        }
        assert!(
            !analysis.analyses[0].findings.iter().any(|finding| finding.check == "parameter-sensitive"),
            "{case:?}"
        );
    }
}

#[tokio::test]
async fn an_unresolved_native_plan_retains_original_counters_without_qualifying_a_workload() {
    let analysis = analyze_case(QueryCohortCase::MissingPlan).await;
    assert!(analysis.analyses[0].workload.is_none());
    for reason in [CollectedAnalysisLimit::MissingPlanDefinition, CollectedAnalysisLimit::RuntimeDenominatorUnavailable]
    {
        assert!(analysis.analyses[0].limitations.iter().any(|limit| limit.reason == reason));
    }
    let native = analysis
        .context
        .records
        .iter()
        .filter_map(|row| match &row.row.record {
            TelemetryRecord::QueryRuntime(runtime) => Some(runtime),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(native.iter().map(|row| row.executions.as_str()).collect::<Vec<_>>(), ["1", "9", "40"]);
    let unresolved = native.iter().find(|row| row.key.native.qualifiers["plan_id"] == "8").unwrap();
    assert!(unresolved.key.references.plan.as_ref().unwrap().definition_id.is_none());

    let restricted = analyze_case(QueryCohortCase::RestrictedStatement).await;
    assert!(restricted.analyses[0].workload.is_none());
    assert!(
        restricted.analyses[0]
            .limitations
            .iter()
            .any(|limit| limit.reason == CollectedAnalysisLimit::QueryTextUnavailable)
    );
    assert_eq!(
        restricted
            .context
            .records
            .iter()
            .filter(|row| matches!(row.row.record, TelemetryRecord::QueryRuntime(_)))
            .count(),
        3
    );
}

#[tokio::test]
async fn an_unretrieved_runtime_fragment_withholds_the_denominator_instead_of_improving_the_query() {
    let (source, records) = query_cohort_evidence(QueryCohortCase::PlanVariation);
    let fixture = Fixture::new(source, records).await;
    fixture.remove_first_runtime();
    let capture = fixture.read().await;
    assert!(!capture.is_read_complete());
    let analysis = collected_evidence::analyze(&capture, None).unwrap();
    assert!(analysis.analyses[0].workload.is_none());
    assert!(
        analysis.analyses[0]
            .limitations
            .iter()
            .any(|limit| limit.reason == CollectedAnalysisLimit::RuntimeDenominatorUnavailable)
    );
    assert!(!analysis.analyses[0].findings.iter().any(|finding| finding.check == "parameter-sensitive"));
}

#[tokio::test]
async fn separate_native_intervals_do_not_append_or_recount_an_earlier_capture() {
    let mut row_ids = vec![];
    for interval_id in ["native-period-1", "native-period-2"] {
        let (mut source, records) = query_cohort_evidence(QueryCohortCase::PlanVariation);
        let SourceObservation::QueryHistory(history) = &mut source.observation else { unreachable!() };
        history.interval.native_id = interval_id.into();
        let fixture = Fixture::new(source, records).await;
        let capture = fixture.read().await;
        let analysis = collected_evidence::analyze(&capture, None).unwrap();
        let workload = analysis.analyses[0].workload.as_ref().unwrap();
        assert_eq!((workload.total_executions, workload.total_time_ms), (50.0, 9500.0));
        row_ids.push(
            analysis
                .context
                .records
                .iter()
                .filter(|row| matches!(row.row.record, TelemetryRecord::QueryRuntime(_)))
                .map(|row| row.row.record_id.clone().unwrap())
                .collect::<std::collections::BTreeSet<_>>(),
        );
    }
    assert!(row_ids[0].is_disjoint(&row_ids[1]));
}
