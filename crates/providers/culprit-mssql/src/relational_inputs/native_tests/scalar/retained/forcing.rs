//! A configured forcing episode is independent context, never proof that forcing was applied or caused work.
use super::*;
use crate::MssqlProvider;
use culprit_collector::capture::{CollectionCancellation, SourceFactory};
use culprit_core::{
    Environment, Provider, QueryStoreScope, ScenarioKind, SourceOutcome, SourceState,
    plan_compare::NativeRuntimeInputRelation,
};
use serde_json::{Value, json};
use std::time::Duration;

mod metadata;

const ACTOR_RETIRE_TIMEOUT: Duration = Duration::from_secs(5);
const SOURCE_DISPATCH_CAP: u32 = 3 * 19 + 12;
// Existing guarded fixture: admission2 + database setup4 + Probe setup9 + Query Store/procedure work6
// + four actual capture owners12 + original cache metadata10 + selected Query Store metadata3
// + live/closed actor verification6 + database teardown3 + at most11 stats-session preparations.
const FIXTURE_DISPATCH_CAP: u32 = 2 + 4 + 9 + 6 + 12 + 10 + 3 + 6 + 3 + 11;

struct Factory(MssqlProvider);
#[async_trait::async_trait]
impl SourceFactory for Factory {
    async fn open(&self, request: &CollectionRequest) -> culprit_core::Result<Box<dyn TelemetrySource>> {
        self.0.telemetry_source(request)
    }
}

struct Episode {
    retained: Original,
    configured_before: Captured,
    configured_after: Captured,
    unforced: Captured,
    counterpart: RecordedRun,
    summary: Value,
}

async fn capture_tuning(owner: &mut OwnedCaptures, factory: &Factory) -> Captured {
    let capture = owner
        .capture_with_factory(
            CollectionSelection::Snapshot { dataset: SnapshotDataset::Tuning },
            &CollectionCancellation::default(),
            factory,
        )
        .await
        .unwrap();
    assert_eq!(capture.manifest.status, CollectionStatus::Complete);
    assert_eq!(capture.source().unwrap().continuity, SourceContinuity::Unverified);
    let context = NativeReadContext::from_snapshot(capture.source().unwrap()).unwrap();
    assert!(context.has_qualified_local_owner());
    assert_eq!(context.target.database().unwrap().incarnation, None);
    assert!(
        matches!(context.partition,SourcePartition::InstanceLocal{member:Some(ref member),..} if member.kind==SharedResourceKind::Instance)
    );
    assert!(capture.manifest.source_queries <= 19);
    let recipe = crate::telemetry::recipes().into_iter().find(|recipe| recipe.id == "mssql.tuning").unwrap();
    assert_eq!(recipe.version, 2);
    assert_eq!(recipe.cost, RecipeCostQualification::Unqualified);
    capture
}

fn forcing<'a>(capture: &'a Captured, plan: &NativePlanIdentity) -> &'a NativeForcedPlan {
    let scope = capture.source().unwrap().observation.snapshot().unwrap();
    let coverage = scope.sources.iter().find(|source| source.source == culprit_core::source_id::FORCED_PLANS).unwrap();
    assert_eq!(coverage.records, 1);
    let values = capture
        .records
        .iter()
        .filter_map(|row| match &row.record {
            TelemetryRecord::Tuning(TuningObservation { fact: TuningFact::ForcedPlan(value), .. }) => Some(value),
            _ => None,
        })
        .collect::<Vec<_>>();
    let [native] = values.as_slice() else { panic!("one configured native forcing point") };
    assert_eq!(native.query_id, plan.query.query_id);
    assert_eq!(native.plan_id, plan.plan_id);
    match native.plan_scope {
        QueryStoreScope::Local => {
            assert!(coverage.complete_scope);
            assert_eq!(coverage.state, SourceState::Collected);
        }
        QueryStoreScope::Unknown => {
            assert!(!coverage.complete_scope);
            assert_eq!(coverage.state, SourceState::Partial);
        }
        _ => panic!("the global forcing flag cannot establish a replica-specific forcing location"),
    }
    assert_eq!(native.forcing.as_deref(), Some("MANUAL"));
    assert_eq!(native.failures, "0");
    assert_eq!(native.reset, culprit_core::ResetMarker::Unknown);
    native
}

fn require_unforced(capture: &Captured, original_scope: &QueryStoreScope) {
    let snapshot = capture.source().unwrap().observation.snapshot().unwrap();
    let coverage =
        snapshot.sources.iter().find(|source| source.source == culprit_core::source_id::FORCED_PLANS).unwrap();
    match original_scope {
        QueryStoreScope::Local => {
            assert!(coverage.complete_scope);
            assert_eq!(coverage.state, SourceState::Empty);
        }
        QueryStoreScope::Unknown => {
            assert!(!coverage.complete_scope);
            assert_eq!(coverage.state, SourceState::Partial);
        }
        _ => panic!("the original forcing point has no qualified replica location"),
    }
    assert_eq!(coverage.records, 0);
    assert!(!capture.records.iter().any(|row| matches!(
        &row.record,
        TelemetryRecord::Tuning(TuningObservation { fact: TuningFact::ForcedPlan(_), .. })
    )));
}

fn tuning_report(capture: &Captured) -> culprit_core::TuningReport {
    let rows = capture
        .records
        .iter()
        .map(|row| match &row.record {
            TelemetryRecord::Tuning(row) => row,
            _ => panic!("original Tuning record"),
        })
        .collect::<Vec<_>>();
    let coverage = capture
        .source()
        .unwrap()
        .observation
        .snapshot()
        .unwrap()
        .sources
        .iter()
        .map(|source| SourceOutcome {
            source: source.source.clone(),
            state: source.state,
            reason: None,
            reason_message: None,
        })
        .collect::<Vec<_>>();
    let report = culprit_checks::tuning::explain(&rows, &coverage).unwrap();
    assert!(report.limitations.contains(&culprit_core::message!("view.tuning.limit.execution")));
    assert!(report.limitations.contains(&culprit_core::message!("view.tuning.limit.management")));
    report
}

async fn change_forcing(probe: &mut Probe, plan: &NativePlanIdentity, force: bool) {
    let procedure = if force { "sys.sp_query_store_force_plan" } else { "sys.sp_query_store_unforce_plan" };
    let mut query = tiberius::Query::new(format!(
        "/* culprit */ /* fixture owned Query Store change only */ DECLARE @result int;EXEC @result={procedure} @query_id=@P1,@plan_id=@P2;SELECT @result AS command_return;"
    ));
    query.bind(plan.query.query_id.parse::<i64>().unwrap());
    query.bind(plan.plan_id.parse::<i64>().unwrap());
    let rows = query
        .query(&mut probe.application)
        .await
        .unwrap()
        .into_results()
        .await
        .unwrap()
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    let [row] = rows.as_slice() else { panic!("one drained owned forcing command outcome") };
    assert_eq!(row.get::<i32, _>("command_return"), Some(0));
}

async fn exercise(probe: &mut Probe, fixture: &OwnedSourceFixture, database: &str) -> Box<Episode> {
    batch(
        &mut probe.application,
        "ALTER DATABASE CURRENT SET QUERY_STORE=ON (OPERATION_MODE=READ_WRITE,QUERY_CAPTURE_MODE=ALL)",
    )
    .await;
    let prime = probe.record_native(2, CacheExpectation::New).await;
    prime.operator("Key Lookup");
    batch(&mut probe.application, "EXEC sys.sp_query_store_flush_db").await;
    let identity = metadata::read(probe, &prime, None).await;
    change_forcing(probe, &identity, true).await;
    let factory = Factory(MssqlProvider {
        endpoint_host: "127.0.0.1".into(),
        endpoint_port: fixture.port,
        environment: Environment::Development,
        pool: probe.pool.clone(),
    });
    let mut owner = OwnedCaptures::from_pool(probe.pool.clone(), probe.port).unwrap();
    let configured_before = capture_tuning(&mut owner, &factory).await;
    let forcing_scope = forcing(&configured_before, &identity).plan_scope.clone();
    // Do not infer cache reuse from the first execution after a configured change. Warm once, then
    // require the existing full original cache-incarnation checks for the incident execution.
    let warmed = probe.record_native(1, CacheExpectation::New).await;
    let original = probe.record_native(1, CacheExpectation::Reused(&warmed.context)).await;
    assert_eq!(original.plan.facts().parameter_context.as_ref().unwrap().retrieved_from_cache, Some(true));
    let mut retained = Original::capture_owned(owner, &original).await;
    verify_actor_present(fixture, database, &probe.actor()).await;
    let configured_after = capture_tuning(&mut retained.owner, &factory).await;
    assert_eq!(forcing(&configured_after, &identity).plan_scope, forcing_scope);
    assert_eq!(identity, metadata::read(probe, &prime, Some(&identity)).await);
    change_forcing(probe, &identity, false).await;
    batch(&mut probe.application, "EXEC sys.sp_recompile N'dbo.scalar_probe'").await;
    let counterpart = probe.record_native(1, CacheExpectation::New).await;
    let unforced = capture_tuning(&mut retained.owner, &factory).await;
    require_unforced(&unforced, &forcing_scope);
    assert_eq!(identity, metadata::read(probe, &prime, Some(&identity)).await);
    assert_eq!(original.result, counterpart.result);
    assert_eq!(prime.plan.statement, original.plan.statement);
    let pair = crate::plan_xml::runtime_input_pair(&original.plan, &counterpart.plan);
    let input_relation = pair.relation();
    let original_hash = original.plan.facts().query_hash.as_ref().expect("original native query hash");
    let counterpart_hash = counterpart.plan.facts().query_hash.as_ref().expect("counterpart native query hash");
    if original_hash.eq_ignore_ascii_case(counterpart_hash) {
        assert_eq!(input_relation, NativeRuntimeInputRelation::Same);
    } else {
        // A forced native plan can expose a different hash. Equal fixture arguments and statement
        // text cannot repair the product's missing native query-identity proof.
        assert_eq!(input_relation, NativeRuntimeInputRelation::Unavailable);
    }
    let lookup = original.operator("Key Lookup");
    let scan = counterpart.operator("Clustered Index Scan");
    let lookup_reads = original.logical_reads("Key Lookup");
    let scan_reads = counterpart.logical_reads("Clustered Index Scan");
    assert!(lookup_reads >= 1000.0 && lookup_reads >= 2.0 * scan_reads && lookup_reads - scan_reads >= 1000.0);
    assert_eq!(lookup.object, scan.object);
    assert_eq!(lookup.facts().table_alias, scan.facts().table_alias);
    assert_eq!(lookup.act_executions, Some(original.result as f64));
    assert_eq!(scan.act_rows, Some(counterpart.result as f64));
    let comparison = culprit_core::plan_compare::compare_native_pair(
        pair,
        "Recorded forcing context",
        "After unforce and explicit recompile",
        &Default::default(),
    );
    assert!(!comparison.same_shape);
    assert!(!comparison.changes.is_empty());
    let work_scenario = comparison.scenarios.iter().find(|scenario| {
        scenario.kind == ScenarioKind::Parameters && scenario.a == Some(lookup.id) && scenario.b == Some(scan.id)
    });
    if input_relation == NativeRuntimeInputRelation::Unavailable {
        assert!(
            work_scenario.is_none(),
            "unequal native query identities cannot establish a same-input reuse scenario"
        );
    }
    if let Some(scenario) = work_scenario {
        assert!(scenario.detail.as_ref().unwrap().contains(&culprit_core::t!("core.plan_compare.scalar_reuse.limit")));
    }
    // The retained lightweight plan is a separate native observation. Its execution counts do not
    // borrow logical I/O counters from the original STATISTICS XML response.
    let observed_lookup = retained.explain();
    let retained_lookup = observed_lookup
        .nodes()
        .into_iter()
        .find(|node| node.op == "Key Lookup")
        .expect("the original retained plan keeps the measured lookup operator");
    assert_eq!(retained_lookup.act_executions, Some(original.result as f64));
    for capture in [&configured_before, &configured_after, &unforced] {
        assert_eq!(capture.source().unwrap().target, retained.capture.source().unwrap().target);
        assert_eq!(capture.source().unwrap().partition, retained.capture.source().unwrap().partition);
    }
    let before = configured_before.source().unwrap().observation.snapshot().unwrap();
    let after = configured_after.source().unwrap().observation.snapshot().unwrap();
    let plan = retained.capture.source().unwrap().observation.snapshot().unwrap();
    assert!(ObservationBounds::new(&before.observed_until, &plan.observed_from).is_some());
    assert!(ObservationBounds::new(&plan.observed_until, &after.observed_from).is_some());
    let summary = json!({"query_store_plan_identity":identity,"same_deliberately_supplied_fixture_input":true,
        "same_supported_native_runtime_inputs":input_relation==NativeRuntimeInputRelation::Same,"same_result":true,
        "original_native_query_hash":original_hash,"counterpart_native_query_hash":counterpart_hash,
        "lookup_node":lookup.id,"lookup_operator_reads":lookup_reads,"scan_node":scan.id,"scan_operator_reads":scan_reads,
        "retained_lookup_actual_executions":retained_lookup.act_executions,
        "retained_lookup_logical_reads":retained_lookup.runtime.as_ref().and_then(|runtime|runtime.logical_reads),
        "qualified_lookup_comparison_present":work_scenario.is_some(),"comparison":comparison,
        "configured_forcing_before":tuning_report(&configured_before),"configured_forcing_after":tuning_report(&configured_after),"unforced_configuration":tuning_report(&unforced),
        "owned_application_executions":4,"owned_actual_plan_capture_dispatches":12,"owned_cache_metadata_dispatches":10,
        "selected_query_store_metadata_dispatches":3,"query_store_setup_and_force_unforce_dispatches":4,"explicit_procedure_recompile_dispatches":1,
        "live_actor_generation_verification_dispatches":2,"live_actor_generation_verifier_session_setup_dispatches":1,
        "configured_forcing_plan_scope":forcing_scope,"forcing_location_qualified":forcing_scope==QueryStoreScope::Local,
        "continuous_forcing_between_observations_established":false,"configured_forcing_applied_to_execution_established":false,"forcing_isolated_as_work_cause":false,"exclusive_parameter_cause":false,
        "fresh_compilation_inferred_from_cache_flag":false,"native_work_is_partial_self_observation":true,"native_cost_qualified":false});
    batch(&mut probe.application, "DROP PROCEDURE dbo.scalar_probe").await;
    assert_eq!(factory.0.source_shutdown(), SourceShutdown::Quiescent);
    Box::new(Episode { retained, configured_before, configured_after, unforced, counterpart, summary })
}

async fn forced_context(fixture: &OwnedSourceFixture, database: &str) {
    let mut probe = Box::new(Box::pin(Probe::counted(fixture, database, IndexCoverage::LookupRequired)).await);
    let actor = probe.actor();
    let observed = AssertUnwindSafe(Box::pin(exercise(&mut probe, fixture, database))).catch_unwind().await;
    // No followup actor SQL runs after a failure. Physical retirement is not successful SQL cancellation.
    let retired =
        AssertUnwindSafe(tokio::time::timeout(ACTOR_RETIRE_TIMEOUT, probe.application.close())).catch_unwind().await;
    let mut episode = match observed {
        Ok(episode) => episode,
        Err(original) => std::panic::resume_unwind(original),
    };
    let retired = match retired {
        Ok(retired) => retired,
        Err(original) => std::panic::resume_unwind(original),
    };
    retired.expect("original forcing actor retirement is bounded").unwrap();
    episode.retained.verify_after_actor_disappears(fixture, database, actor).await;
    for capture in [&episode.configured_before, &episode.configured_after, &episode.unforced] {
        episode.retained.owner.verify_retained(capture).await.unwrap();
    }
    let recovered = episode.retained.explain();
    assert_eq!(
        crate::plan_xml::runtime_input_pair(&recovered, &episode.counterpart.plan).relation(),
        NativeRuntimeInputRelation::Unavailable
    );
    let captures =
        [&episode.configured_before, &episode.retained.capture, &episode.configured_after, &episode.unforced];
    let queries = captures.iter().map(|capture| capture.manifest.source_queries).sum::<u32>();
    assert!(queries <= SOURCE_DISPATCH_CAP);
    episode.summary["original_manifests"] = json!(captures.iter().map(|capture| &capture.manifest).collect::<Vec<_>>());
    episode.summary["original_records"] = json!(captures.iter().map(|capture| &capture.records).collect::<Vec<_>>());
    episode.summary["source_queries"] = json!(queries);
    episode.summary["source_dispatch_cap"] = json!(SOURCE_DISPATCH_CAP);
    episode.summary["separately_paid_fixture_dispatch_cap_including_session_preparation"] = json!(FIXTURE_DISPATCH_CAP);
    episode.summary["fixture_caps_are_not_observed_server_work"] = json!(true);
    episode.summary["successful"] = json!(true);
    crate::test_support::save_cost_receipt(
        &crate::test_support::receipt_directory(),
        "scalar-original-forcing-control",
        &episode.summary,
    )
    .unwrap();
}

#[tokio::test]
#[ignore = "requires SQL2022 nonce-owned scalar fixture; Query Store forcing is owned fixture setup only"]
async fn owned_forcing_context_keeps_original_work_and_native_configuration_after_unforce_without_exclusive_reuse_blame()
 {
    Box::pin(super::super::super::fixture::run("scalar_forced", forced_context)).await;
}
