use super::fixture::Fixture;
use culprit_collector::analytical::test_support::{QueryCohortCase, query_cohort_evidence};
use culprit_core::{
    QueryStat,
    collected_analysis::{CollectedAnalysisLimit, CollectedQueryAnalysis},
    telemetry::*,
};
use culprit_engine::collected_evidence;

fn one(analysis: &CollectedQueryAnalysis) -> &QueryStat {
    let workload =
        analysis.analyses[0].workload.as_ref().unwrap_or_else(|| panic!("{:?}", analysis.analyses[0].limitations));
    let [query] = workload.queries.as_slice() else { panic!("one native query population") };
    query
}

fn runtime<'a>(records: &'a mut [TelemetryRecord], fragment: &str) -> &'a mut RuntimeMeasurement {
    records
        .iter_mut()
        .find_map(|row| match row {
            TelemetryRecord::QueryRuntime(row) if row.key.native.id == fragment => Some(row),
            _ => None,
        })
        .unwrap()
}

fn plan_id(records: &[TelemetryRecord], native: &str) -> String {
    records
        .iter()
        .find_map(|row| match row {
            TelemetryRecord::PlanDefinition(plan) if plan.identity.plan_id == native => Some(plan.id.clone()),
            _ => None,
        })
        .unwrap()
}

#[tokio::test]
async fn per_plan_extrema_and_optional_costs_require_every_original_fragment() {
    let (source, mut records) = query_cohort_evidence(QueryCohortCase::PlanVariation);
    let slow = plan_id(&records, "47");
    let fast = plan_id(&records, "8");
    let fragment = runtime(&mut records, "fragment-2");
    fragment.metrics[0].minimum = None;
    fragment.metrics[0].maximum = None;
    fragment.first_execution_at = None;
    fragment.last_execution_at = None;
    fragment.metrics.retain(|metric| metric.key != RuntimeMetricKey::CpuTime);
    let fixture = Fixture::new(source, records).await;
    let analysis = collected_evidence::analyze(&fixture.read().await, None).unwrap();
    let query = one(&analysis);
    assert_eq!(query.executions, 50.0);
    assert_eq!(query.total_time_ms, 9500.0);
    assert_eq!(query.mean_time_ms, 190.0);
    assert_eq!(query.cpu_time_ms, None);
    assert_eq!(query.max_time_ms, None);
    assert_eq!(query.spread.as_ref().unwrap().min_ms, None);
    assert!(query.first_seen.is_none() && query.last_seen.is_none());
    let slow = query.plans.iter().find(|plan| plan.id == slow).unwrap();
    assert_eq!(slow.executions, 10.0);
    assert_eq!(slow.total_time_ms, 9100.0);
    assert_eq!(slow.mean_time_ms, 910.0);
    assert!(slow.min_time_ms.is_none() && slow.max_time_ms.is_none());
    assert!(slow.first_seen.is_none() && slow.last_seen.is_none());
    assert_eq!(slow.stddev_time_ms, None, "mixed native deviations have no declared pooling convention");
    assert_eq!(slow.cpu_time_ms, None);
    assert_eq!(slow.is_forced, None);
    let fast = query.plans.iter().find(|plan| plan.id == fast).unwrap();
    assert_eq!(fast.executions, 40.0);
    assert_eq!(fast.min_time_ms, Some(10.0));
    assert_eq!(fast.max_time_ms, Some(10.0));
    assert_eq!(fast.stddev_time_ms, Some(0.0));
    assert_eq!(fast.cpu_time_ms, Some(200.0));
    assert_eq!(fast.is_forced, None);
}

#[tokio::test]
async fn unknown_native_qualifiers_remain_distinct_without_a_declared_cohort() {
    let (source, mut records) = query_cohort_evidence(QueryCohortCase::UnknownCohort);
    runtime(&mut records, "fragment-2").key.native.qualifiers.insert("unrecognized-reset-token".into(), "later".into());
    let fixture = Fixture::new(source, records).await;
    let analysis = collected_evidence::analyze(&fixture.read().await, None).unwrap();
    let workload = analysis.analyses[0].workload.as_ref().unwrap();
    assert_eq!(workload.queries.len(), 3);
    assert_eq!(workload.total_time_ms, 9500.0);
    assert_eq!(workload.total_executions, 50.0);
    assert_eq!(workload.query_count, 1.0);
    assert!(workload.queries.iter().all(|query| query.plans.len() == 1));
    assert!(
        analysis.analyses[0].query_inputs.iter().all(|input| input.measurements.iter().all(|key| key.cohort.is_none()))
    );
    assert!(!analysis.analyses[0].findings.iter().any(|finding| finding.check == "parameter-sensitive"));
}

#[tokio::test]
async fn authenticated_cohort_equality_authorizes_fragments_without_removing_raw_identity() {
    let (source, mut records) = query_cohort_evidence(QueryCohortCase::PlanVariation);
    for id in ["fragment-1", "fragment-2", "fragment-3"] {
        runtime(&mut records, id).key.native.qualifiers.insert("source-row-token".into(), id.into());
    }
    let fixture = Fixture::new(source, records).await;
    let analysis = collected_evidence::analyze(&fixture.read().await, None).unwrap();
    assert_eq!(one(&analysis).plans.len(), 2);
    let [input] = analysis.analyses[0].query_inputs.as_slice() else { panic!() };
    assert!(input.references.plan.is_none(), "a multi-plan group has no single original plan reference");
    assert_eq!(input.measurements.len(), 3);
    for key in &input.measurements {
        assert_eq!(key.native.qualifiers["source-row-token"], key.native.id);
        assert!(key.cohort.is_some());
        assert!(key.references.query.is_some() && key.references.plan.is_some());
    }
    assert_eq!(one(&analysis).observed_wait_ms(WaitClass::Lock), Some(30.0));
}

#[tokio::test]
async fn a_wait_for_a_third_plan_cannot_join_through_the_query_cohort_alone() {
    let (source, mut records) = query_cohort_evidence(QueryCohortCase::PlanVariation);
    let mut third = records
        .iter()
        .find_map(|row| match row {
            TelemetryRecord::PlanDefinition(plan) => Some(plan.clone()),
            _ => None,
        })
        .unwrap();
    third.identity.plan_id = "third-plan-without-runtime".into();
    third.id = third.identity.definition_id(&source).unwrap();
    let mut wait = records
        .iter()
        .find_map(|row| match row {
            TelemetryRecord::QueryWaits(wait) => Some(wait.clone()),
            _ => None,
        })
        .unwrap();
    wait.key.native.id = "unmatched-native-wait".into();
    wait.key.native.qualifiers.insert("plan_id".into(), third.identity.plan_id.clone());
    wait.key.references.plan =
        Some(NativeDefinitionReference { native: third.identity.native(), definition_id: Some(third.id.clone()) });
    wait.total = MeasurementValue::Integer("4000".into());
    records.extend([TelemetryRecord::PlanDefinition(third), TelemetryRecord::QueryWaits(wait)]);
    let fixture = Fixture::new(source, records).await;
    let analysis = collected_evidence::analyze(&fixture.read().await, None).unwrap();
    let query = one(&analysis);
    assert_eq!(query.plans.len(), 2);
    assert_eq!(query.observed_wait_ms(WaitClass::Lock), Some(30.0));
    assert_eq!(query.measured_wait_ms(WaitClass::Lock), None);
    assert!(
        analysis.analyses[0].limitations.iter().any(|limit| limit.reason == CollectedAnalysisLimit::WaitWithoutRuntime)
    );
    assert!(analysis.context.records.iter().any(|record| matches!(&record.row.record,
        TelemetryRecord::QueryWaits(wait) if wait.key.native.id == "unmatched-native-wait")));
}

#[tokio::test]
async fn mismatched_wait_cohort_keeps_only_the_original_plan_positive() {
    let (source, mut records) = query_cohort_evidence(QueryCohortCase::PlanVariation);
    for record in &mut records {
        if let TelemetryRecord::QueryWaits(wait) = record
            && wait.key.native.id == "wait-2"
        {
            wait.key.cohort.as_mut().unwrap().qualifiers.insert("reset-generation".into(), "another".into());
        }
    }
    let fixture = Fixture::new(source, records).await;
    let analysis = collected_evidence::analyze(&fixture.read().await, None).unwrap();
    let query = one(&analysis);
    assert_eq!(query.observed_wait_ms(WaitClass::Lock), Some(25.0));
    assert_eq!(query.measured_wait_ms(WaitClass::Lock), None);
    assert!(
        analysis.analyses[0].limitations.iter().any(|limit| limit.reason == CollectedAnalysisLimit::WaitWithoutRuntime)
    );
}

#[tokio::test]
async fn zero_execution_rows_do_not_authorize_a_plans_independent_waits() {
    let (source, mut records) = query_cohort_evidence(QueryCohortCase::PlanVariation);
    for id in ["fragment-1", "fragment-2"] {
        runtime(&mut records, id).executions = "0".into();
    }
    let fixture = Fixture::new(source, records).await;
    let analysis = collected_evidence::analyze(&fixture.read().await, None).unwrap();
    let query = one(&analysis);
    assert_eq!(query.executions, 40.0);
    assert_eq!(query.total_time_ms, 400.0);
    assert_eq!(query.plans.len(), 1);
    assert_eq!(query.observed_wait_ms(WaitClass::Lock), Some(5.0));
    assert_eq!(query.measured_wait_ms(WaitClass::Lock), None);
    assert!(
        analysis.analyses[0].limitations.iter().any(|limit| limit.reason == CollectedAnalysisLimit::WaitWithoutRuntime)
    );
}

#[tokio::test]
async fn a_missing_plan_definition_keeps_its_runtime_without_inventing_a_variant_or_reference() {
    let (source, records) = query_cohort_evidence(QueryCohortCase::MissingPlan);
    let fixture = Fixture::new(source, records).await;
    let analysis = collected_evidence::analyze(&fixture.read().await, None).unwrap();
    let query = one(&analysis);
    assert_eq!(query.executions, 50.0);
    assert_eq!(query.total_time_ms, 9500.0);
    assert_eq!(query.plans.len(), 1);
    assert_eq!(query.plans[0].executions, 10.0);
    assert!(
        analysis.analyses[0]
            .limitations
            .iter()
            .any(|limit| limit.reason == CollectedAnalysisLimit::MissingPlanDefinition)
    );
    let [input] = analysis.analyses[0].query_inputs.as_slice() else { panic!() };
    assert!(input.references.plan.is_none());
    assert_eq!(input.measurements.len(), 3);
    assert!(
        input.measurements.iter().any(|key| key.references.plan.as_ref().is_some_and(|plan| plan.native.id == "8"))
    );
}

#[tokio::test]
async fn a_missing_runtime_fragment_withholds_the_denominator_even_after_cohort_projection() {
    let (source, records) = query_cohort_evidence(QueryCohortCase::PlanVariation);
    let fixture = Fixture::new(source, records).await;
    fixture.remove_first_runtime();
    let capture = fixture.read().await;
    let analysis = collected_evidence::analyze(&capture, None).unwrap();
    assert!(analysis.analyses[0].workload.is_none());
    assert!(
        analysis.analyses[0]
            .limitations
            .iter()
            .any(|limit| limit.reason == CollectedAnalysisLimit::RuntimeDenominatorUnavailable)
    );
    assert_eq!(analysis.analyses[0].query_inputs[0].measurements.len(), 2);
    assert!(
        analysis.context.records.iter().any(|record| matches!(&record.row.record, TelemetryRecord::QueryRuntime(_)))
    );
}

#[cfg(feature = "yugabyte")]
#[tokio::test]
async fn query_history_uses_its_authenticated_yugabyte_parser_for_unmarked_native_json() {
    let (mut source, mut records) = query_cohort_evidence(QueryCohortCase::PlanVariation);
    let CollectionTarget::Database(database) = &mut source.target else { panic!() };
    database.engine = culprit_core::Engine::Yugabyte;
    let mut ids = std::collections::BTreeMap::new();
    for record in &mut records {
        match record {
            TelemetryRecord::QueryDefinition(query) => {
                let id = query.identity.definition_id(&source).unwrap();
                ids.insert(query.id.clone(), id.clone());
                query.id = id;
            }
            TelemetryRecord::PlanDefinition(plan) => {
                let id = plan.identity.definition_id(&source).unwrap();
                ids.insert(plan.id.clone(), id.clone());
                plan.id = id;
                plan.query_definition_id = Some(plan.identity.query.definition_id(&source).unwrap());
                plan.plan = DefinitionPayload::plan(
                    include_str!("../../../providers/culprit-yugabyte/tests/data/qpm.json"),
                    culprit_core::Engine::Yugabyte,
                );
            }
            _ => (),
        }
    }
    for record in &mut records {
        let key = match record {
            TelemetryRecord::QueryRuntime(row) => &mut row.key,
            TelemetryRecord::QueryWaits(row) => &mut row.key,
            _ => continue,
        };
        for reference in [&mut key.references.query, &mut key.references.plan].into_iter().flatten() {
            let original = reference.definition_id.as_ref().unwrap();
            reference.definition_id = Some(ids[original].clone());
        }
    }
    let fixture = Fixture::new(source, records).await;
    let capture = fixture.read().await;
    assert!(capture.is_read_complete(), "{:?}", capture.gaps());
    let analysis = collected_evidence::analyze(&capture, None).unwrap();
    let query = one(&analysis);
    let plan = query.plan.as_ref().unwrap();
    assert_eq!(plan.engine, culprit_core::Engine::Yugabyte);
    assert!(plan.facts().estimated_cost_unavailable);
    assert!(plan.nodes().iter().all(|node| node.facts().estimated_rows_unavailable));
    assert!(plan.root.act_rows.is_none() && plan.facts().elapsed_ms.is_none());
    assert_eq!(query.plans.len(), 2);
    assert_eq!(query.executions, 50.0);
    assert_eq!(query.total_time_ms, 9500.0);
    assert!(
        !analysis.analyses[0].limitations.iter().any(|limit| limit.reason == CollectedAnalysisLimit::PlanUnavailable)
    );
}

#[cfg(feature = "postgres")]
#[tokio::test]
async fn real_native_heap_fetch_advice_keeps_its_evidence_but_no_executable_sql_in_either_archive_output() {
    let native_plan = include_str!("../insights/pg-heap-fetches.json");
    let (source, mut records) = query_cohort_evidence(QueryCohortCase::PlanVariation);
    for record in &mut records {
        match record {
            TelemetryRecord::QueryDefinition(query) => {
                query.sql = DefinitionPayload::sql(
                    "SELECT customer_id FROM public.events WHERE customer_id < 2000",
                    culprit_core::Engine::Postgres,
                );
            }
            TelemetryRecord::PlanDefinition(plan) => {
                plan.plan = DefinitionPayload::plan(native_plan, culprit_core::Engine::Postgres);
            }
            _ => (),
        }
    }
    let fixture = Fixture::new(source, records).await;
    let capture = fixture
        .read_families_with_recovery(
            &[
                culprit_collector::analytical::evidence::EvidenceFamily::QueryDefinitions,
                culprit_collector::analytical::evidence::EvidenceFamily::PlanDefinitions,
                culprit_collector::analytical::evidence::EvidenceFamily::QueryRuntime,
                culprit_collector::analytical::evidence::EvidenceFamily::QueryWaits,
            ],
            culprit_collector::analytical::evidence::RecoveryPayloadBudget::default(),
        )
        .await;
    assert!(capture.is_read_complete());
    let analysis = collected_evidence::analyze(&capture, None).unwrap();
    let archived = one(&analysis);
    assert_eq!(archived.executions, 50.0);
    let insight = archived
        .plan
        .as_ref()
        .unwrap()
        .insights
        .as_ref()
        .unwrap()
        .items
        .iter()
        .find(|insight| insight.rule == "heap-fetches")
        .unwrap();
    assert!(insight.fix_sql.is_none());
    assert!(!insight.fix.is_empty() && !insight.verify.is_empty());
    assert!(
        analysis.analyses[0].limitations.iter().any(|limit| limit.record_id.as_deref() == Some(archived.id.as_str())
            && limit.reason == CollectedAnalysisLimit::ExecutableFixPrerequisitesUnknown)
    );
    assert!(analysis.context.records.iter().any(|row| matches!(&row.row.record,
        TelemetryRecord::PlanDefinition(plan) if matches!(&plan.plan,
            DefinitionPayload::Available {text,..} if text.contains("Heap Fetches")))));

    // A separate full saved scan establishes the prerequisites that the two archive captures do not carry. Its
    // material, recurring query goes through the real pure pipeline and genuinely produces this executable SQL.
    let mut scan: culprit_core::Snapshot =
        serde_json::from_str(include_str!("../../../../fixtures/postgres-demo.json")).unwrap();
    let mut current = scan
        .workload
        .as_ref()
        .unwrap()
        .queries
        .iter()
        .find(|query| query.executions > 1.0 && query.mean_time_ms >= culprit_core::insights::MATERIAL_MS)
        .unwrap()
        .clone();
    current.text = "SELECT customer_id FROM public.events WHERE customer_id < 2000".into();
    current.plan = Some(culprit_engine::plans::parse_native_plan(native_plan, culprit_core::Engine::Postgres).unwrap());
    current.plan_history = None;
    current.plans.clear();
    current.last_seen = Some(scan.taken_at.clone());
    scan.workload.as_mut().unwrap().queries = vec![current];
    let direct = culprit_checks::analyze(scan);
    let direct = direct.snapshot.workload.as_ref().unwrap().queries[0]
        .plan
        .as_ref()
        .unwrap()
        .insights
        .as_ref()
        .unwrap()
        .items
        .iter()
        .find(|insight| insight.rule == "heap-fetches")
        .unwrap();
    assert!(direct.fix_sql.as_deref().is_some_and(|sql| sql.starts_with("VACUUM")));

    let (source, mut records) =
        culprit_collector::analytical::test_support::object_definition_evidence_cases().remove(2);
    for record in &mut records {
        if let TelemetryRecord::ObjectDefinition(row) = record
            && let ObjectDefinitionFact::RetainedPlan(native) = &mut row.fact
        {
            native.definition.plan = DefinitionPayload::plan(native_plan, culprit_core::Engine::Postgres);
            *row = ObjectDefinitionRecord::new(row.source.clone(), row.fact.clone()).unwrap();
        }
    }
    let fixture = Fixture::new(source, records).await;
    let capture = fixture
        .read_families_with_recovery(
            &[culprit_collector::analytical::evidence::EvidenceFamily::ObjectDefinitions],
            culprit_collector::analytical::evidence::RecoveryPayloadBudget::default(),
        )
        .await;
    let plans = culprit_engine::collected_resources::cached_plans(&capture).unwrap();
    let retained = plans.report.entries.iter().find_map(|entry| entry.plan.as_ref()).unwrap();
    let insight =
        retained.insights.as_ref().unwrap().items.iter().find(|insight| insight.rule == "heap-fetches").unwrap();
    assert!(insight.fix_sql.is_none());
    assert_eq!(insight.fix, direct.fix);
    assert_eq!(insight.verify, direct.verify);
    assert!(plans.context.records.iter().any(|row| matches!(&row.row.record,
        TelemetryRecord::ObjectDefinition(row) if matches!(&row.fact,
            ObjectDefinitionFact::RetainedPlan(plan) if matches!(&plan.definition.plan,
                DefinitionPayload::Available {text,..} if text.contains("Heap Fetches"))))));
}
