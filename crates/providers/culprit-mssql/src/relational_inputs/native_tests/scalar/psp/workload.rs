//! Native Query Store populations remain separate children. The real shared producer owns all aggregation;
//! the fixture only compares its output to the original native sufficient statistics and invokes the real check.
use super::*;
use crate::{
    query_store::runtime::PlanSummary,
    read::{NativeRead, ReadSession},
    test_support::recorded_runtime::{RecordedRuntime, same_duration},
};
use culprit_collector::capture::CollectionCancellation;
use culprit_core::{Capability, Environment, ServerInfo, Snapshot, SnapshotOptions};
use std::{collections::BTreeSet, sync::Arc};

pub(super) async fn read(
    pool: &Arc<crate::conn::Pool>,
    port: u16,
    database: &str,
    object: i64,
    cancellation: &CollectionCancellation,
) -> Box<Snapshot> {
    // The complete population read owns its one reader through the final close; callers retain a pointer.
    Box::pin(async {
        assert!(!cancellation.is_cancelled());
        let budget = CollectionBudget { max_queries: 64, total_timeout_ms: 15_000, ..CollectionBudget::default() };
        let native = ReadSession::new(pool.clone(), budget.clone());
        let recording = RecordedRuntime::new(native);
        let rows = recording.rows();
        let mut reader = crate::query_store::Reader::new(recording);
        let result = AssertUnwindSafe(crate::test_support::owned_case::until_cancelled(cancellation, async {
            let probe = crate::probe::run(&mut reader).await?;
            assert_eq!(probe.major, 16, "this experiment is SQL2022 PSP, not a claim for later versions");
            let server =
                crate::probe::server_info(&probe, vec![Capability::QueryStore, Capability::QueryPlans], vec![]);
            let workload = crate::workload::from_query_store(
                &mut reader,
                &SnapshotOptions { window_minutes: 60, plans: 16, top_queries: 128, ..Default::default() },
            )
            .await?;
            Ok((server, workload))
        }))
        .catch_unwind()
        .await;
        let queries = reader.queries();
        assert_eq!(
            reader.close().await,
            SourceShutdown::Quiescent,
            "same source owner closes after all post-read checks"
        );
        drop(reader);
        let (server, result) = match result {
            Ok(result) => result.unwrap(),
            Err(error) => std::panic::resume_unwind(error),
        };
        assert!(queries <= budget.max_queries);
        assert_eq!(result.database.as_ref().unwrap().database, database);
        let original = rows.lock().unwrap();
        let child_ids = assert_family_populations(&result, &original, object);
        let identity = result.database.as_ref().unwrap().identity("127.0.0.1", port);
        let mut snapshot = snapshot(server, result.workload);
        snapshot.bind_identity(identity);
        snapshot.waits = result.waits;
        result.coverage.append_to(&mut snapshot);
        let check = culprit_checks::all().into_iter().find(|check| check.info().id == "parameter-sensitive").unwrap();
        // Each real child reaches the count policy. One plan per native child remains one plan; this does not
        // fabricate cross-child averages or claim that all database parameter evidence is complete/healthy.
        assert!(
            check
                .run(&snapshot)
                .iter()
                .all(|finding| finding.queries.iter().all(|query| !child_ids.contains(&query.id))),
            "healthy native children are not a plan-switch regression"
        );
        snapshot
    })
    .await
}
fn assert_family_populations(
    result: &crate::workload::WorkloadResult,
    original: &[PlanSummary],
    object: i64,
) -> BTreeSet<String> {
    let children = result
        .workload
        .queries
        .iter()
        .filter(|query| {
            query
                .variant_family
                .as_ref()
                .is_some_and(|family| family.parent.qualifiers.get("object_id") == Some(&object.to_string()))
        })
        .collect::<Vec<_>>();
    assert_eq!(children.len(), 2, "the bounded two-class workload must genuinely generate two native PSP variants");
    let mut dispatchers = BTreeSet::new();
    let mut runs = 0.0;
    let mut child_ids = BTreeSet::new();
    for child in children {
        child_ids.insert(child.id.clone());
        let family = child.variant_family.as_ref().unwrap();
        assert!(family.is_valid());
        assert_eq!(child.native_identity.as_ref(), Some(&family.variant));
        assert_eq!(result.query_identities.get(&child.id), Some(&family.variant));
        assert!(
            !original.iter().any(|row| row.identity == family.parent),
            "native dispatcher runtime must genuinely be absent"
        );
        assert!(
            !result.workload.queries.iter().any(|query| query.native_identity.as_ref() == Some(&family.parent)),
            "no synthetic dispatcher population"
        );
        let native = original.iter().filter(|row| row.identity == family.variant).collect::<Vec<_>>();
        assert_eq!(
            native.len(),
            1,
            "one real plan per child is a healthy routing control, not two plans for one population"
        );
        assert_eq!(child.executions, probe::CALLS_PER_VARIANT as f64);
        assert_eq!(child.executions, native.iter().map(|row| row.moments.executions).sum::<u64>() as f64);
        same_duration(child.total_time_ms, native.iter().map(|row| row.moments.duration_us).sum());
        assert_eq!(child.plans.len(), 1);
        assert_eq!(child.plans[0].native_identity.as_ref(), Some(&native[0].plan_identity));
        assert_eq!(child.plans[0].executions, child.executions);
        same_duration(child.plans[0].total_time_ms, native[0].moments.duration_us);
        runs += child.executions;
        dispatchers.insert(serde_json::to_string(&family.dispatcher).unwrap());
    }
    assert_eq!(runs, (probe::CALLS_PER_VARIANT * 2) as f64);
    assert_eq!(dispatchers.len(), 1);
    assert_eq!(result.workload.total_executions, original.iter().map(|row| row.moments.executions).sum::<u64>() as f64);
    same_duration(result.workload.total_time_ms, original.iter().map(|row| row.moments.duration_us).sum());
    child_ids
}
fn snapshot(server: ServerInfo, workload: culprit_core::Workload) -> Box<Snapshot> {
    Box::new(Snapshot {
        operations: Default::default(),
        format: culprit_core::SNAPSHOT_FORMAT,
        id: "owned-psp-native".into(),
        connection: "owned-psp-native".into(),
        environment: Environment::Development,
        taken_at: chrono::Utc::now().to_rfc3339(),
        duration_ms: 0.0,
        server,
        runtime_populations: workload.population_catalog.clone(),
        workload: Some(workload),
        tables: vec![],
        indexes: vec![],
        columns: vec![],
        foreign_keys: vec![],
        missing_indexes: vec![],
        statistics: vec![],
        waits: vec![],
        resources: vec![],
        settings: vec![],
        notes: vec![],
        pressure: None,
        options: None,
        query_families: vec![],
        plan_cache: None,
        tuning: None,
        deadlocks: None,
        constraints: vec![],
        histograms: None,
        escalations_before: None,
        compilations_before: None,
        contention_before: None,
        evictions_before: None,
        sizes_before: None,
        sources: vec![],
        identity: None,
    })
}
