use super::*;
use crate::{
    query_store::{
        Reader,
        replicas::tests::row as replica,
        tests::{definition_row, summary_row},
        window_sql::tests::{layout_row, row as window_row},
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
        final_window,
        Ok(vec![layout()]),
        Ok(final_catalog),
    ]
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
    assert_eq!(reader.queries(), 12, "later groups reuse datasets and the original incarnation-qualified text");
    assert!(reader.is_population_batch());
    assert_eq!(reader.close().await, SourceShutdown::Quiescent);
}

#[tokio::test]
async fn empty_or_partial_readable_groups_do_not_start_an_unrelated_native_cache_read() {
    let mut script = replies(Ok(vec![window()]), groups());
    script[3] = Ok(Vec::new());
    let _ = script.drain(5..7); // No selected query definitions exist in this native empty population.
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
    assert_eq!(reader.queries(), 12);
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
    assert_eq!(reader.queries(), 12);
    assert_eq!(reader.close().await, SourceShutdown::Quiescent);
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
    assert_eq!(reader.queries(), 10, "no layout/catalog SQL follows the failed owner check");
    assert_eq!(reader.close().await, SourceShutdown::Unconfirmed);
}
