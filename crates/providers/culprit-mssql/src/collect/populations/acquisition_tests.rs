use super::*;
use crate::{
    query_store::{
        Reader,
        replicas::tests::row as replica,
        tests::{definition_row, summary_row},
        window_sql::tests::{azure_row, layout_row, row as window_row},
    },
    read::ReadResult,
    resources::test_support::{FakeRead, replace},
    value::{Rec, Val},
};

fn layout() -> Rec {
    replace(&layout_row(), &[("replicas", Val::Int(1)), ("waits", Val::Int(0))])
}
fn groups() -> Vec<Rec> {
    vec![replica(1, 1, None), replica(7, 2, Some("read-member"))]
}
fn window() -> Rec {
    replace(&window_row(), &[("wait_stats_capture_mode_desc", Val::Null)])
}
fn runtime(group: i64, duration: f64) -> Rec {
    replace(
        &summary_row(),
        &[("replica_group_id", Val::Int(group)), ("duration_us", Val::Float(duration)), ("is_forced", Val::Null)],
    )
}
fn lengths(definition: &Rec) -> Rec {
    Rec::of(
        &definition
            .columns()
            .filter(|(key, _)| matches!(*key, "id" | "source_bytes"))
            .map(|(key, value)| (key, value.clone()))
            .collect::<Vec<_>>(),
    )
}
fn options() -> SnapshotOptions {
    SnapshotOptions { window_minutes: 60, top_queries: 1, plans: 0, ..Default::default() }
}
fn replies(final_window: ReadResult<Vec<Rec>>, final_catalog: Vec<Rec>) -> Vec<ReadResult<Vec<Rec>>> {
    let definition = definition_row("SELECT 1");
    vec![
        Ok(vec![layout()]),
        Ok(groups()),
        Ok(vec![window()]),
        Ok(vec![runtime(1, 4_000.0), runtime(7, 40_000.0)]),
        Ok(vec![]),
        Ok(vec![lengths(&definition)]),
        Ok(vec![definition]),
        Ok(vec![Rec::of(&[("minutes", Val::Int(60))])]),
        Ok(vec![]),
        Ok(vec![crate::query_store::tests::compilation_row()]),
        Ok(vec![crate::signals::tests::database_compilation_row()]),
        final_window,
        Ok(vec![layout()]),
        Ok(final_catalog),
    ]
}

fn azure_window() -> Rec {
    replace(&azure_row(), &[("wait_stats_capture_mode_desc", Val::Null)])
}

fn azure_replies(final_window: Rec) -> Vec<ReadResult<Vec<Rec>>> {
    let mut script = replies(Ok(vec![final_window]), groups());
    let azure_layout = replace(&layout(), &[("engine_edition", Val::Int(5))]);
    script[0] = Ok(vec![azure_layout.clone()]);
    script[2] = Ok(vec![azure_window()]);
    script[12] = Ok(vec![azure_layout]);
    script
}

#[tokio::test]
async fn compiler_policy_is_read_once_on_workload_owner_then_published_only_after_authority_and_quiet_close() {
    let sql = "SELECT id FROM dbo.orders WHERE id=987654321 OPTION (RECOMPILE)";
    let row = definition_row(sql);
    let mut columns = row.columns().map(|(name, value)| (name, value.clone())).collect::<Vec<_>>();
    columns.extend([("engine_internal", Val::Int(0)), ("cloud_internal", Val::Int(0))]);
    let definition = Rec::of(&columns);
    for outcome in ["quiet", "changed", "cancelled", "unconfirmed"] {
        let final_window = if outcome == "changed" {
            replace(&window(), &[("database_name", Val::Text("another-db".into()))])
        } else {
            window()
        };
        let mut script = replies(Ok(vec![final_window]), groups());
        script[5] = Ok(vec![lengths(&definition)]);
        script[6] = Ok(vec![definition.clone()]);
        script[9] = if outcome == "cancelled" {
            Err(crate::read::ReadError::Stopped(CollectionStop::Cancelled))
        } else {
            Ok(vec![replace(
                &crate::query_store::tests::compilation_row(),
                &[
                    ("query_initial_compile_at", Val::Text("2020-01-01T06:00:00Z".into())),
                    ("count_compiles", Val::Int(12)),
                    ("avg_compile_duration", Val::Float(500_000.0)),
                ],
            )])
        };
        let mut native = FakeRead::new(script);
        if outcome == "unconfirmed" {
            native.shutdown = SourceShutdown::Unconfirmed;
        }
        let mut reader = Reader::new(&mut native);
        let mut workloads = acquire(&mut reader, &options()).await;
        let shutdown = reader.close().await;
        workloads.finish_compilation(60, shutdown);
        let mut snapshot: Snapshot =
            serde_json::from_str(include_str!("../../../../../../fixtures/mssql-demo.json")).unwrap();
        let mut coverage = Coverage::default();
        workloads.take_compilation().apply(&mut snapshot, &mut coverage);
        assert_eq!(native.closed, 1);
        assert_eq!(
            native.calls.len(),
            if outcome == "cancelled" {
                10
            } else if outcome == "changed" {
                12
            } else {
                14
            }
        );
        if outcome == "quiet" {
            let raw = snapshot.operations.query_compilation.unwrap();
            let culprit_core::telemetry::OperationalFact::MssqlQueryCompilation(fact) = &raw.observations[0].fact
            else {
                panic!()
            };
            assert_eq!(fact.compilations, "12");
            assert!(fact.statement.is_available());
            assert!(workloads.populations.iter().all(|(_, part)| {
                part.workload.as_ref().unwrap().queries[0].costs.as_ref().unwrap().compile_history.is_some()
            }));
        } else {
            assert!(snapshot.operations.query_compilation.is_none(), "{outcome}");
            assert!(workloads.participants().iter().all(|participant| {
                participant.query.costs.as_ref().is_none_or(|costs| costs.compile_history.is_none())
            }));
        }
    }
}

#[tokio::test]
async fn completed_query_store_identity_replaces_endpoint_only_snapshot_identity_for_every_exact_group() {
    let mut reader = Reader::new(FakeRead::new(azure_replies(azure_window())));
    let workloads = acquire(&mut reader, &options()).await;
    assert_eq!(reader.close().await, SourceShutdown::Quiescent);
    let mut snapshot: Box<Snapshot> =
        serde_json::from_str(include_str!("../../../../../../fixtures/mssql-demo.json")).unwrap();
    snapshot.options = Some(options());
    snapshot.workload = None;
    let endpoint =
        culprit_core::DatabaseIdentity::server(Engine::Mssql, "culprit_demo", "saved-alias.invalid", 1433, None);
    snapshot.bind_identity(endpoint.clone());
    let identity = workloads.database_identity(&snapshot.server.database, "saved-alias.invalid", 1433).unwrap();
    assert_ne!(identity.telemetry_id().unwrap(), endpoint.telemetry_id().unwrap());
    assert_eq!(identity.incarnation.as_deref(), Some("logical-database-guid"));
    let alias = workloads.database_identity(&snapshot.server.database, "another-alias.invalid", 1433).unwrap();
    assert_eq!(identity.telemetry_id().unwrap(), alias.telemetry_id().unwrap());
    assert!(workloads.database_identity("another-database", "saved-alias.invalid", 1433).is_err());
    snapshot.bind_identity(identity.clone());
    let batch = workloads.project(*snapshot, &options()).unwrap();
    assert_eq!(batch.requested.identity.as_ref(), Some(&identity));
    assert!(batch.requested.workload.is_none(), "the unselected overview does not fabricate pooled history");
    for snapshot in &batch.populations {
        assert_eq!(snapshot.identity.as_ref(), Some(&identity));
        assert_eq!(snapshot.workload_measurement().unwrap().database, identity);
        let query = &snapshot.workload.as_ref().unwrap().queries[0];
        assert_eq!(
            query.native_identity.as_ref(),
            Some(&crate::query_store::decode::query_identity(&summary_row()).unwrap())
        );
    }
    assert_eq!(reader.queries(), 14, "logical identity brackets the same two compiler reads within the held owner");
}

#[tokio::test]
async fn changed_database_physical_incarnation_or_role_withholds_every_group_before_publication_or_fallback() {
    for (field, value) in [
        ("database_name", "another-database"),
        ("incarnation", "recreated-logical-database"),
        ("physical_incarnation", "moved-physical-database"),
        ("updateability", "READ_ONLY"),
    ] {
        let changed = replace(&azure_window(), &[(field, Val::Text(value.into()))]);
        let mut reader = Reader::new(FakeRead::new(azure_replies(changed)));
        let workloads = acquire(&mut reader, &options()).await;
        assert!(workloads.requested.workload.is_none());
        assert!(workloads.populations.iter().all(|(_, part)| part.workload.is_none() && part.database.is_none()));
        assert!(!workloads.allows_cache_fallback());
        assert_eq!(CollectionStop::from_error(reader.retirement_reason().unwrap()), CollectionStop::SourceChanged);
        assert_eq!(
            reader.queries(),
            12,
            "{field}: no catalog, borrowed result or fresh source follows the failed identity check"
        );
        assert_eq!(reader.close().await, SourceShutdown::Quiescent);
    }
}

#[tokio::test]
async fn completed_unknown_azure_identity_keeps_history_but_cannot_promote_an_endpoint_namespace() {
    let unknown = replace(
        &azure_window(),
        &[("native_identity_rows", Val::Int(0)), ("incarnation", Val::Null), ("physical_incarnation", Val::Null)],
    );
    let mut script = azure_replies(unknown.clone());
    script[2] = Ok(vec![unknown]);
    let mut reader = Reader::new(FakeRead::new(script));
    let workloads = acquire(&mut reader, &options()).await;
    assert!(workloads.populations.iter().all(|(_, part)| part.workload.is_some()));
    assert!(!workloads.allows_cache_fallback());
    let identity = workloads.database_identity("culprit_demo", "saved-alias.invalid", 1433).unwrap();
    assert!(identity.incarnation.is_none());
    assert_eq!(
        identity,
        culprit_core::DatabaseIdentity::server(Engine::Mssql, "culprit_demo", "saved-alias.invalid", 1433, None)
    );
    assert_eq!(reader.queries(), 14);
    assert_eq!(reader.close().await, SourceShutdown::Quiescent);
}

#[tokio::test]
async fn every_group_uses_its_own_runtime_with_one_shared_definition_and_final_native_authority_check() {
    let mut reader = Reader::new(FakeRead::new(replies(Ok(vec![window()]), groups())));
    let workloads = acquire(&mut reader, &options()).await;
    assert!(workloads.requested.workload.is_none(), "a multi-group default never becomes a pooled workload");
    assert!(!workloads.allows_cache_fallback(), "unselected overview is not a reason to start another native source");
    assert_eq!(workloads.populations.len(), 2);
    assert_eq!(workloads.populations[0].1.workload.as_ref().unwrap().total_time_ms, 4.0);
    assert_eq!(workloads.populations[1].1.workload.as_ref().unwrap().total_time_ms, 40.0);
    for (population, part) in &workloads.populations {
        let workload = part.workload.as_ref().unwrap();
        assert_eq!(workload.population.as_ref(), Some(population));
        assert_eq!(workload.queries[0].text, "SELECT 1");
        assert_eq!(workload.window_end.as_deref(), Some("2020-01-01T07:00:00Z"));
        assert_eq!(workload.window_hours, 1.0);
    }
    assert_eq!(reader.queries(), 14, "all groups share the original datasets and one compilation acquisition");
    assert!(reader.is_population_batch());
    assert_eq!(reader.close().await, SourceShutdown::Quiescent);
}

#[tokio::test]
async fn empty_or_partial_readable_groups_do_not_start_an_unrelated_native_cache_read() {
    let mut script = replies(Ok(vec![window()]), groups());
    script[3] = Ok(Vec::new());
    let _ = script.drain(5..7); // No selected query definitions exist in this native empty population.
    let _ = script.drain(7..9); // No query participants require compilation history.
    let mut reader = Reader::new(FakeRead::new(script));
    let workloads = acquire(&mut reader, &options()).await;
    assert!(workloads.populations.iter().all(|(_, part)| {
        part.workload.as_ref().is_some_and(|workload| workload.queries.is_empty())
            && part.outcome.outcomes().iter().any(|source| {
                source.source == sources::QUERY_STORE.name && source.state == culprit_core::SourceState::Empty
            })
    }));
    assert!(!workloads.allows_cache_fallback(), "a measured empty history is still a completed source read");
    assert_eq!(reader.queries(), 10, "only the shared Query Store datasets and real final checks ran");
    assert_eq!(reader.close().await, SourceShutdown::Quiescent);

    let mut reader = Reader::new(FakeRead::new(replies(Ok(vec![window()]), groups())));
    let mut workloads = acquire(&mut reader, &options()).await;
    let error = culprit_core::Error::new(culprit_core::ErrorCode::PermissionDenied, "fixture native detail denied");
    workloads.populations[0].1.outcome.partial(&sources::QUERY_STORE, error.message_source());
    workloads.populations[1].1 = failed_part(&error, workloads.catalog.clone());
    assert!(workloads.populations[0].1.workload.is_some());
    assert!(workloads.populations[1].1.workload.is_none());
    assert!(!workloads.allows_cache_fallback(), "one useful partial group prevents redundant default cache work");
    assert_eq!(reader.queries(), 14);
    assert_eq!(reader.close().await, SourceShutdown::Quiescent);
}

#[tokio::test]
async fn cache_fallback_remains_available_when_all_groups_have_a_nonterminal_native_read_failure() {
    let error = culprit_core::Error::new(culprit_core::ErrorCode::PermissionDenied, "fixture Query Store read denied");
    let mut reader = Reader::new(FakeRead::new(vec![
        Ok(vec![layout()]),
        Ok(groups()),
        Ok(vec![window()]),
        Err(crate::read::ReadError::Source(error)),
        Ok(vec![window()]),
        Ok(vec![layout()]),
        Ok(groups()),
    ]));
    let workloads = acquire(&mut reader, &options()).await;
    assert!(workloads.requested.workload.is_none());
    assert!(workloads.populations.iter().all(|(_, part)| part.workload.is_none()));
    assert!(!workloads.stop_followup);
    assert!(workloads.allows_cache_fallback(), "real source failure can still use the existing independent fallback");
    assert_eq!(reader.queries(), 7, "the failed dataset is not retried once per group");
    assert_eq!(reader.close().await, SourceShutdown::Quiescent);
}

#[tokio::test]
async fn any_final_catalog_change_withholds_all_provisional_populations_and_forbids_a_fresh_cache_reader() {
    let mut reader = Reader::new(FakeRead::new(replies(Ok(vec![window()]), vec![replica(1, 1, None)])));
    let workloads = acquire(&mut reader, &options()).await;
    assert_eq!(workloads.catalog.as_ref().unwrap().populations.len(), 2, "original alternatives remain inspectable");
    assert!(workloads.populations.iter().all(|(_, part)| part.workload.is_none()));
    assert!(!workloads.allows_cache_fallback());
    assert!(workloads.requested.outcome.outcomes().iter().any(|source| {
        source.source == sources::QUERY_STORE.name && source.state == culprit_core::SourceState::Unavailable
    }));
    assert_eq!(CollectionStop::from_error(reader.retirement_reason().unwrap()), CollectionStop::SourceChanged);
    assert!(!reader.is_population_batch(), "provisional datasets/definitions are released on native authority loss");
    assert_eq!(reader.queries(), 14);
    assert_eq!(reader.close().await, SourceShutdown::Quiescent);
}

#[tokio::test]
async fn empty_duplicate_or_malformed_final_authority_keeps_its_original_failure_and_forbids_fallback() {
    for (rows, expected) in [
        (vec![], crate::read::invalid_data()),
        (vec![window(), window()], CollectionStop::RowLimit.error()),
        (
            vec![replace(&window(), &[("window_clock", Val::Text("not-a-native-clock".into()))])],
            crate::read::invalid_data(),
        ),
    ] {
        let mut reader = Reader::new(FakeRead::new(replies(Ok(rows), groups())));
        let mut workloads = acquire(&mut reader, &options()).await;
        assert!(workloads.stop_followup);
        assert!(!workloads.allows_cache_fallback(), "a failed final proof cannot start an independent cache read");
        assert!(workloads.pending_compilation.is_none());
        assert!(workloads.requested.workload.is_none());
        assert!(workloads.populations.iter().all(|(_, part)| part.workload.is_none() && part.database.is_none()));
        for part in std::iter::once(&workloads.requested).chain(workloads.populations.iter().map(|(_, part)| part)) {
            let outcomes = part.outcome.outcomes();
            let source = outcomes.iter().find(|source| source.source == sources::QUERY_STORE.name).unwrap();
            assert_eq!(source.state, culprit_core::SourceState::Unavailable);
            assert_eq!(source.reason.as_deref(), Some(expected.message.as_str()));
        }
        let mut snapshot: Snapshot =
            serde_json::from_str(include_str!("../../../../../../fixtures/mssql-demo.json")).unwrap();
        let mut coverage = Coverage::default();
        workloads.take_compilation().apply(&mut snapshot, &mut coverage);
        assert!(snapshot.operations.query_compilation.is_none());
        let outcomes = coverage.outcomes();
        let compiler = outcomes.iter().find(|source| source.source == sources::QUERY_COMPILATION.name).unwrap();
        assert_eq!(compiler.state, culprit_core::SourceState::Unavailable);
        assert_eq!(compiler.reason.as_deref(), Some(expected.message.as_str()));
        assert_eq!(reader.queries(), 12, "no catalog or fresh native source follows the failed final proof");
        assert_eq!(reader.close().await, SourceShutdown::Quiescent);
    }
}

#[tokio::test]
async fn a_whole_batch_row_cap_keeps_its_original_reason_and_cannot_restart_as_a_plan_cache_fallback() {
    let mut script = vec![Ok(vec![layout_row()])];
    script.extend((0..10).map(|_| Ok(vec![Rec::of(&[]); 10_000])));
    let mut reader = Reader::new(FakeRead::new(script));
    reader.prepare_population_batch(1).await.unwrap();
    for _ in 0..9 {
        reader.read_rows("SELECT /* culprit */ bounded test rows").await.unwrap();
    }
    let error = reader.read_rows("SELECT /* culprit */ bounded test rows").await.unwrap_err();
    assert_eq!(CollectionStop::from_error(&error), CollectionStop::RowLimit);
    let workloads = acquire(&mut reader, &options()).await;
    assert!(!workloads.allows_cache_fallback(), "the retired owner forbids a new native budget/session");
    assert!(workloads.stop_followup);
    let source = workloads
        .requested
        .outcome
        .outcomes()
        .into_iter()
        .find(|source| source.source == sources::QUERY_STORE.name)
        .unwrap();
    assert_eq!(source.state, culprit_core::SourceState::Unavailable);
    assert_eq!(source.reason.as_deref(), Some(error.message.as_str()));
    assert_eq!(reader.queries(), 11, "no discovery or fallback SQL follows the cumulative stop");
    assert_eq!(reader.close().await, SourceShutdown::Quiescent);
}

#[tokio::test]
async fn failed_final_native_verification_preserves_unconfirmed_closure_without_any_later_native_work() {
    let mut native =
        FakeRead::new(replies(Err(crate::read::ReadError::Stopped(CollectionStop::CancellationUnconfirmed)), groups()));
    native.shutdown = SourceShutdown::Unconfirmed;
    let mut reader = Reader::new(native);
    let workloads = acquire(&mut reader, &options()).await;
    assert!(workloads.populations.iter().all(|(_, part)| part.workload.is_none()));
    assert!(!workloads.allows_cache_fallback());
    assert_eq!(reader.queries(), 12, "no layout/catalog SQL follows the failed owner check");
    assert_eq!(reader.close().await, SourceShutdown::Unconfirmed);
}
