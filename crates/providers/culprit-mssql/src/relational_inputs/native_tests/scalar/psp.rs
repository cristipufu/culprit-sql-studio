//! SQL2022 PSP acceptance uses a known eligible native population, complete result semantics and a bounded
//! same-input owned intervention. Variant diversity alone never establishes harmful parameter reuse.
use super::super::*;
use crate::test_support::{OwnedSourceFixture, owned_case::finish_case};
use culprit_collector::capture::CollectionCancellation;
use culprit_core::telemetry::CachedStatementContext;
use std::time::Duration;

mod cache;
mod identity;
mod probe;
mod retained;
mod stack_tests;
mod workload;
use identity::RoutingExpectation;

// The real check admits statements only from50 executions. Each child independently reaches that count;
// the dispatcher never receives the sum. No adaptive row growth, workload retry or deadline reset is permitted.
const CASE_TIMEOUT: Duration = Duration::from_secs(180);

async fn healthy(fixture: &OwnedSourceFixture, database: &str) {
    let cancellation = CollectionCancellation::default();
    let deadline = tokio::time::Instant::now() + CASE_TIMEOUT;
    let (expired,())=finish_case(deadline,&cancellation,Box::pin(async {
        let mut probe=probe::Probe::new(fixture,database,&cancellation).await;
        let common=probe.record(1,RoutingExpectation::Variant,None).await;
        let rare=probe.record(2,RoutingExpectation::Variant,None).await;
        assert_distinct_native_children(&common,&rare);
        for _ in 1..probe::CALLS_PER_VARIANT {
            let common_repeat=probe.record(1,RoutingExpectation::Variant,Some(&common.recorded.context)).await;
            let rare_repeat=probe.record(2,RoutingExpectation::Variant,Some(&rare.recorded.context)).await;
            assert_same_variant(&common,&common_repeat);
            assert_same_variant(&rare,&rare_repeat);
        }
        assert_suitable_native_access(&common,&rare);
        probe.flush().await;
        let snapshot=workload::read(&probe.pool,fixture.port,database,probe.object,&cancellation).await;
        assert_eq!(snapshot.server.database,database);
        // These source originals must really be supported and retained. The native experiment fails if the
        // connected-cache platform, exact variant body or safe proof is unavailable; no Azure header is forged.
        let mut saved=retained::SavedVariants::capture(&probe,fixture.port,&[&common,&rare],&cancellation).await;

        probe.disable_psp().await;
        let plain_small=probe.record(2,RoutingExpectation::Ordinary,None).await;
        let plain_large=probe.record(1,RoutingExpectation::Ordinary,Some(&plain_small.recorded.context)).await;
        assert_same_incarnation(&plain_small.recorded.context,&plain_large.recorded.context);
        assert_ne!(plain_large.recorded.context.identity.key().unwrap(),common.recorded.context.identity.key().unwrap());
        assert_ne!(plain_large.recorded.context.identity.key().unwrap(),rare.recorded.context.identity.key().unwrap());
        assert_eq!(plain_large.recorded.result,common.recorded.result,"identical SQL and input, unchanged data/statistics/indexes");
        assert_eq!(plain_small.recorded.result,rare.recorded.result);
        assert_eq!(plain_large.recorded.plan.facts().parameter_context.as_ref().unwrap().retrieved_from_cache,Some(true));
        assert_eq!(plain_large.recorded.plan.facts().parameter_context.as_ref().unwrap().parameters[0].integer_values_equal,Some(false));
        let access=plain_large.recorded.operator("Index Seek").facts().access_rows.unwrap();
        assert!(access.actual_total.unwrap()>access.estimated_per_execution.unwrap()*100.0,"the control must really retain a bad small-input estimate");
        assert!(plain_large.recorded.logical_reads("Key Lookup")>common.recorded.logical_reads("Clustered Index Scan"),"one actual data-access operator, no inclusive sums or elapsed-time speedup claim");
        assert_no_causal_upgrade(&plain_large.recorded.plan);
        eprintln!("owned PSP:2native children x{}exact-result calls,1dispatcher with no runtime; same-input disabled-PSP control accesses more native lookup pages; cache/context originals stay separate",probe::CALLS_PER_VARIANT);

        let actor=probe.actor.clone();
        crate::test_support::batch(&mut probe.application,"DROP PROCEDURE dbo.parameter_variant_probe").await;
        probe.application.close().await.unwrap();
        saved.verify_after_removal(fixture,database,&actor).await;
    })).await;
    assert!(!expired, "the same body settles after cancellation; an expired case is not qualified");
}
fn assert_same_incarnation(original: &CachedStatementContext, current: &CachedStatementContext) {
    assert_eq!(original.database_id, current.database_id);
    assert_eq!(original.identity.key().unwrap(), current.identity.key().unwrap());
}
fn assert_same_variant(original: &probe::Run, current: &probe::Run) {
    assert_eq!(original.variant, current.variant);
    assert_eq!(original.plan_hash, current.plan_hash);
    assert_eq!(original.recorded.result, current.recorded.result);
    assert_same_incarnation(&original.recorded.context, &current.recorded.context);
    assert_no_causal_upgrade(&current.recorded.plan);
}
fn assert_distinct_native_children(common: &probe::Run, rare: &probe::Run) {
    assert!(common.variant.is_some() && rare.variant.is_some());
    assert_ne!(common.variant, rare.variant);
    assert_ne!(common.plan_hash, rare.plan_hash);
    assert_eq!(common.recorded.context.database_id, rare.recorded.context.database_id);
    assert_ne!(common.recorded.context.identity.key().unwrap(), rare.recorded.context.identity.key().unwrap());
    // SQL Server documents the shared query hash across native variants; it cannot select either child.
    assert_eq!(common.recorded.context.query_hash, rare.recorded.context.query_hash);
}
fn assert_suitable_native_access(common: &probe::Run, rare: &probe::Run) {
    common.recorded.operator("Clustered Index Scan");
    rare.recorded.operator("Index Seek");
    rare.recorded.operator("Key Lookup");
    for run in [common, rare] {
        assert_no_causal_upgrade(&run.recorded.plan);
        assert_native_parameter_comparison(&run.recorded.plan);
        let data = run
            .recorded
            .plan
            .nodes()
            .into_iter()
            .filter(|node| matches!(node.op.as_str(), "Index Seek" | "Clustered Index Scan"))
            .collect::<Vec<_>>();
        let [data] = data.as_slice() else { panic!("one native base access per variant") };
        let rows = data.facts().access_rows.unwrap();
        let (estimated, actual) = (rows.estimated_per_execution.unwrap(), rows.actual_total.unwrap());
        assert!(
            estimated > 0.0 && actual > 0.0 && estimated <= actual * 2.0 && actual <= estimated * 2.0,
            "healthy routed input must actually have a suitable native estimate"
        );
    }
}
fn assert_native_parameter_comparison(plan: &Plan) {
    use culprit_core::plan_facts::{PlanParameterIssue, PlanParameterValueState};
    let context = plan
        .facts()
        .parameter_context
        .as_ref()
        .expect("the selected actual variant must supply original safe parameter facts");
    assert!(context.is_valid() && !context.issues.contains(&PlanParameterIssue::UnboundStatement));
    let [parameter] = context.parameters.as_slice() else { panic!("one exact declared native integer input") };
    assert_eq!(parameter.data_type.as_deref(), Some("int"));
    assert_eq!(parameter.compiled, PlanParameterValueState::Recorded);
    assert_eq!(parameter.runtime, PlanParameterValueState::Recorded);
    assert!(
        parameter.integer_values_equal.is_some(),
        "native value comparison is preserved, never borrowed from fixture inputs"
    );
}
fn assert_no_causal_upgrade(plan: &Plan) {
    assert!(culprit_core::insights::insights(plan).iter().filter(|insight| insight.rule == "parameter-context").all(
        |insight| insight.severity == culprit_core::Severity::Low && insight.impact == 0.0 && insight.fix_sql.is_none()
    ));
}

#[tokio::test]
#[ignore = "requires SQL2022 nonce-owned loopback fixture; exact variants,100application calls and2owned controls; no production replay/cost qualification"]
async fn owned_healthy_psp_variants_preserve_semantics_native_populations_and_original_retained_proofs() {
    super::super::fixture::run("psp_healthy", healthy).await;
}
