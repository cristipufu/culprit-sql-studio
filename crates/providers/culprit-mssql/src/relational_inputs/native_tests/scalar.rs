//! Scalar replay and recompilation run only in the admitted disposable fixture. They qualify the experiment,
//! not automatic production replay or a causal verdict from a cache lifetime/count or representation difference.
use super::*;
use crate::test_support::{OwnedSourceFixture, verify_owned_source};
use culprit_core::{Severity, plan_facts::PlanParameterComparison};

mod probe;
mod psp;
mod retained;
use probe::{CacheExpectation, IndexCoverage, Probe, RecordedRun};

#[derive(Clone, Copy)]
enum Direction {
    SmallToLarge,
    LargeToSmall,
}
impl Direction {
    fn initial(self) -> i32 {
        match self {
            Self::SmallToLarge => 2,
            Self::LargeToSmall => 1,
        }
    }
    fn affected(self) -> i32 {
        match self {
            Self::SmallToLarge => 1,
            Self::LargeToSmall => 2,
        }
    }
    fn label(self) -> &'static str {
        match self {
            Self::SmallToLarge => "small_large",
            Self::LargeToSmall => "large_small",
        }
    }
}

#[derive(Clone, Copy)]
enum ParameterValues {
    Same,
    Different,
}
fn assert_context(run: &RecordedRun, expected: ParameterValues) {
    let different = matches!(expected, ParameterValues::Different);
    let context = run.plan.facts().parameter_context.as_ref().expect("selected actual native parameter context");
    let [parameter] = context.parameters.as_slice() else { panic!("one exact scalar native parameter") };
    assert_eq!(parameter.data_type.as_deref(), Some("int"));
    assert_eq!(parameter.integer_values_equal, Some(!different));
    assert_eq!(
        parameter.comparison,
        if different {
            PlanParameterComparison::DifferentRepresentation
        } else {
            PlanParameterComparison::SameRepresentation
        }
    );
    if different {
        assert_eq!(context.retrieved_from_cache, Some(true), "the native statement must report reuse");
    }
    for insight in
        culprit_core::insights::insights(&run.plan).iter().filter(|insight| insight.rule == "parameter-context")
    {
        assert_eq!(insight.severity, Severity::Low);
        assert_eq!(insight.impact, 0.0);
        assert!(insight.fix_sql.is_none(), "retained co-occurrence does not authorize forcing or recompilation");
    }
}

async fn compare(fixture: &OwnedSourceFixture, database: &str, direction: Direction) {
    let mut probe = Probe::new(fixture, database, IndexCoverage::LookupRequired).await;
    let original = probe.record(direction.initial(), CacheExpectation::New).await;
    let affected = probe.record(direction.affected(), CacheExpectation::Reused(&original.context)).await;
    assert_eq!(original.context.identity.key().unwrap(), affected.context.identity.key().unwrap());
    assert_context(&original, ParameterValues::Same);
    assert_context(&affected, ParameterValues::Different);
    match direction {
        Direction::SmallToLarge => {
            assert!(
                affected.operator("Index Seek").facts().access_rows.unwrap().actual_total.unwrap()
                    > affected.operator("Index Seek").facts().access_rows.unwrap().estimated_per_execution.unwrap()
                        * 100.0
            );
            affected.operator("Key Lookup");
        }
        Direction::LargeToSmall => {
            affected.operator("Clustered Index Scan");
        }
    }

    // Source collection sees the actual retained native entry before any intervention. Its Outbox-owned original
    // survives later cache/request removal, including the safe pre-redaction comparison proof.
    let mut retained = retained::Original::capture(&probe, &affected).await;
    batch(&mut probe.application, "EXEC sys.sp_recompile N'dbo.scalar_probe'").await;
    let fresh = probe.record(direction.affected(), CacheExpectation::New).await;
    assert_ne!(affected.context.identity.key().unwrap(), fresh.context.identity.key().unwrap());
    assert_eq!(affected.result, fresh.result, "identical input and SQL must retain identical semantics");
    assert_context(&fresh, ParameterValues::Same);
    match direction {
        Direction::SmallToLarge => assert!(
            affected.logical_reads("Key Lookup") > fresh.logical_reads("Clustered Index Scan"),
            "the predicted repeated data access must actually decrease under the owned intervention"
        ),
        Direction::LargeToSmall => assert!(
            affected.logical_reads("Clustered Index Scan") > fresh.logical_reads("Key Lookup"),
            "the fresh small-input plan must avoid the original full data scan"
        ),
    }
    eprintln!(
        "owned scalar {}: native same-entry reuse and changed-entry fresh compile, identical affected result; CPU/elapsed independently retained {:?}/{:?} -> {:?}/{:?}; no inclusive counter sum",
        direction.label(),
        affected.plan.facts().cpu_ms,
        affected.plan.facts().elapsed_ms,
        fresh.plan.facts().cpu_ms,
        fresh.plan.facts().elapsed_ms
    );
    let actor = probe.actor();
    batch(&mut probe.application, "DROP PROCEDURE dbo.scalar_probe").await;
    probe.application.close().await.unwrap();
    retained.verify_after_actor_disappears(fixture, database, actor).await;
}

async fn small_to_large(fixture: &OwnedSourceFixture, database: &str) {
    compare(fixture, database, Direction::SmallToLarge).await;
}
async fn large_to_small(fixture: &OwnedSourceFixture, database: &str) {
    compare(fixture, database, Direction::LargeToSmall).await;
}

#[tokio::test]
#[ignore = "requires SQL2022 nonce-owned loopback fixture; no production replay or native cost qualification"]
async fn owned_small_to_large_scalar_reuse_and_same_input_fresh_compilation_keep_original_retained_proofs() {
    super::fixture::run("scalar_sl", small_to_large).await;
}
#[tokio::test]
#[ignore = "requires SQL2022 nonce-owned loopback fixture; no production replay or native cost qualification"]
async fn owned_large_to_small_scalar_reuse_and_same_input_fresh_compilation_keep_original_retained_proofs() {
    super::fixture::run("scalar_ls", large_to_small).await;
}

async fn healthy(fixture: &OwnedSourceFixture, database: &str) {
    let mut probe = Probe::new(fixture, database, IndexCoverage::Covering).await;
    let original = probe.record(2, CacheExpectation::New).await;
    let different_value = probe.record(3, CacheExpectation::Reused(&original.context)).await;
    assert_eq!(original.result, different_value.result, "different rare values have the same actual input population");
    assert_context(&different_value, ParameterValues::Different);
    let more_input = probe.record(1, CacheExpectation::Reused(&original.context)).await;
    assert!(more_input.result > original.result);
    assert_context(&more_input, ParameterValues::Different);
    for _ in 0..3 {
        let repeat = probe.record(2, CacheExpectation::Reused(&original.context)).await;
        assert_context(&repeat, ParameterValues::Same);
        assert_eq!(repeat.result, original.result);
        assert!(
            !culprit_core::insights::insights(&repeat.plan).iter().any(|insight| insight.rule == "parameter-context")
        );
    }
    // A covering access serves these different demands without a lookup-to-scan intervention. Native access
    // differences remain useful, but neither varied values, more rows nor more calls establish harmful reuse.
    assert!(more_input.plan.nodes().iter().all(|node| node.op != "Key Lookup"));
    probe.application.close().await.unwrap();
}
#[tokio::test]
#[ignore = "requires SQL2022 nonce-owned loopback fixture; healthy same-plan/different-demand control"]
async fn owned_healthy_scalar_same_plan_different_values_input_volume_and_call_count_never_gain_a_causal_verdict() {
    super::fixture::run("scalar_ok", healthy).await;
}

async fn counted_lookup(fixture: &OwnedSourceFixture, database: &str) {
    let mut probe = Probe::counted(fixture, database, IndexCoverage::LookupRequired).await;
    let original = probe.record(2, CacheExpectation::New).await;
    let affected = probe.record(1, CacheExpectation::Reused(&original.context)).await;
    assert_context(&affected, ParameterValues::Different);
    let seek = affected.operator("Index Seek");
    let lookup = affected.operator("Key Lookup");
    let parameter_context = affected.plan.facts().parameter_context.as_ref().unwrap();
    let [parameter] = parameter_context.parameters.as_slice() else { panic!("one native integer parameter") };
    let usage = parameter_context.uses.iter().find(|usage| usage.node_id == seek.id).unwrap();
    assert_eq!(parameter_context.parameter(usage), Some(parameter));
    let rows = seek.act_rows.unwrap();
    assert_eq!(rows, affected.result as f64, "the native COUNT reads the seeded nonnull input population");
    assert_eq!(lookup.act_rows, Some(rows));
    assert_eq!(lookup.facts().serial_execution_count.as_deref(), Some(rows.to_string().as_str()));
    let reads = affected.logical_reads("Key Lookup");
    assert!(reads >= 1000.0, "the native lookup must record material reads");
    let public = culprit_core::insights::insights(&affected.plan)
        .into_iter()
        .find(|insight| insight.rule == "key-lookups")
        .unwrap();
    let mut ordinary = affected.plan.clone();
    ordinary.facts_mut().parameter_context = None;
    let ordinary =
        culprit_core::insights::insights(&ordinary).into_iter().find(|insight| insight.rule == "key-lookups").unwrap();
    assert_eq!(
        public.fix,
        culprit_core::t!("insight.parameter_lookup.fix", covering = &ordinary.fix),
        "native scope: {:?}",
        affected
            .plan
            .nodes()
            .into_iter()
            .take(16)
            .map(|node| (
                node.id,
                node.kind,
                node.act_rows,
                node.act_executions,
                node.facts().serial_execution_count.as_deref(),
                node.facts().parallel_operator,
                node.facts().estimated_rows_without_goal,
            ))
            .collect::<Vec<_>>()
    );
    assert_eq!(public.verify, culprit_core::t!("insight.parameter_lookup.verify"));
    assert!(public.nodes.contains(&lookup.id) && public.nodes.contains(&seek.id));
    let output = culprit_core::t!(
        "insight.parameter_lookup.detail",
        parameter = culprit_core::t!(
            "insight.parameter_context.named_type",
            parameter = parameter.name.as_deref().unwrap(),
            data_type = parameter.data_type.as_deref().unwrap()
        ),
        estimated = culprit_core::format::count(seek.est_rows),
        actual = culprit_core::format::count(rows),
        node = culprit_core::format::digits(lookup.id.to_string()),
        executions = culprit_core::format::count(rows),
        reads = culprit_core::format::count(reads)
    );
    assert!(public.detail.contains(&output), "the public observation names the actual lookup and its native counts");

    let mut retained = retained::Original::capture(&probe, &affected).await;
    batch(&mut probe.application, "EXEC sys.sp_recompile N'dbo.scalar_probe'").await;
    let fresh = probe.record(1, CacheExpectation::New).await;
    assert_ne!(affected.context.identity.key().unwrap(), fresh.context.identity.key().unwrap());
    assert_eq!(affected.result, fresh.result);
    assert_context(&fresh, ParameterValues::Same);
    assert!(fresh.plan.nodes().iter().all(|node| node.kind != culprit_core::NodeKind::Lookup));
    let fresh_reads = fresh.logical_reads("Clustered Index Scan");
    assert!(fresh_reads < reads, "the same-input native intervention must actually reduce this access work");
    eprintln!(
        "owned counted scalar: lookup node={} executions={rows} logical_reads={reads}; fresh scan reads={fresh_reads}; identical result={}",
        lookup.id, affected.result
    );
    let actor = probe.actor();
    batch(&mut probe.application, "DROP PROCEDURE dbo.scalar_probe").await;
    probe.application.close().await.unwrap();
    retained.verify_after_actor_disappears(fixture, database, actor).await;
}

#[tokio::test]
#[ignore = "requires SQL2022 nonce-owned loopback fixture; exact cached lookup and same-input fresh/retained controls"]
async fn owned_parameterized_lookup_work_uses_exact_native_counts_and_preserves_original_retained_history() {
    super::fixture::run("scalar_lookup", counted_lookup).await;
}
