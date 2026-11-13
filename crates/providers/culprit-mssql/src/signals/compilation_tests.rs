use super::{
    tests::{compilation_identities, database_compilation_row, query_without_costs},
    *,
};
use crate::{
    query_store::tests::compilation_row,
    read::{NativeRead, ReadError},
    resources::test_support::{FakeRead, replace},
    value::Val,
};
use culprit_core::{
    QueryCosts, SourceState,
    signals::CompileHistory,
    telemetry::{CollectionStop, SourceShutdown},
};

#[tokio::test]
async fn matching_native_identity_retains_compile_history_without_borrowing_runtime_lifetime() {
    let row = replace(
        &compilation_row(),
        &[
            ("count_compiles", Val::Int(5)),
            ("compiling_s", Val::Int(7200)),
            ("query_initial_compile_at", Val::Text("2020-01-01T08:00:00.153333300+02:00".into())),
        ],
    );
    let mut queries = vec![query_without_costs()];
    let original_sql = queries[0].text.clone();
    let mut coverage = Coverage::default();
    let signals = collect_query_compilations(
        Reader::new(FakeRead::new(vec![Ok(vec![row]), Ok(vec![database_compilation_row()])])),
        30,
        &mut queries,
        &compilation_identities(),
        &mut coverage,
    )
    .await;
    let costs = queries[0].costs.as_ref().unwrap();
    let history = costs.compile_history.as_ref().unwrap();
    assert_eq!(history.count, 5.0);
    assert!(!history.within_window);
    assert!(costs.plan_ms.is_none() && costs.plans.is_none());
    assert_eq!(history.memory.unwrap().last_kb, Some(8192.0));
    assert_eq!(queries[0].text, original_sql);
    assert_eq!(signals.len(), 2);
    assert_eq!(coverage.outcomes()[0].state, SourceState::Collected);
}

#[tokio::test]
async fn recycled_native_query_qualifiers_cannot_attach_compilation_to_earlier_sql() {
    for (field, changed) in [
        ("context_settings_id", Val::Int(6)),
        ("object_id", Val::Int(123)),
        ("query_hash", Val::Text("FEDCBA9876543210".into())),
        ("query_initial_compile_at", Val::Text("2020-01-01T06:00:01Z".into())),
    ] {
        let changed = replace(&compilation_row(), &[(field, changed)]);
        let mut queries = vec![query_with_previous_compilation()];
        let original_sql = queries[0].text.clone();
        let mut coverage = Coverage::default();
        let signals = collect_query_compilations(
            Reader::new(FakeRead::new(vec![Ok(vec![changed]), Ok(vec![database_compilation_row()])])),
            60,
            &mut queries,
            &compilation_identities(),
            &mut coverage,
        )
        .await;
        assert_compilation_withheld(&queries[0]);
        assert_eq!(queries[0].text, original_sql);
        assert_eq!(coverage.outcomes()[0].state, SourceState::Partial, "{field}");
        assert_eq!(signals.len(), 2, "independent database totals remain their own measured context");
    }
}

#[tokio::test]
async fn absent_definition_or_compilation_stays_unknown_without_retaining_an_old_attachment() {
    for (identities, rows) in [(HashMap::new(), vec![compilation_row()]), (compilation_identities(), vec![])] {
        let mut queries = vec![query_with_previous_compilation()];
        let mut coverage = Coverage::default();
        collect_query_compilations(
            Reader::new(FakeRead::new(vec![Ok(rows), Ok(vec![database_compilation_row()])])),
            60,
            &mut queries,
            &identities,
            &mut coverage,
        )
        .await;
        assert_compilation_withheld(&queries[0]);
        assert_eq!(coverage.outcomes()[0].state, SourceState::Partial);
    }
}

#[tokio::test]
async fn one_unknown_query_does_not_erase_an_independently_matched_compilation() {
    let mut other = query_without_costs();
    other.id = "4".into();
    let mut queries = vec![query_without_costs(), other];
    let mut coverage = Coverage::default();
    collect_query_compilations(
        Reader::new(FakeRead::new(vec![
            Ok(vec![compilation_row(), replace(&compilation_row(), &[("query_id", Val::Int(4))])]),
            Ok(vec![database_compilation_row()]),
        ])),
        60,
        &mut queries,
        &compilation_identities(),
        &mut coverage,
    )
    .await;
    assert!(queries[0].costs.as_ref().unwrap().compile_history.is_some());
    assert!(queries[1].costs.is_none());
    assert_eq!(coverage.outcomes()[0].state, SourceState::Partial);
}

#[tokio::test]
async fn native_timeout_or_cancellation_prevents_followup_sql_and_clears_unadmitted_compilation() {
    for stop in [CollectionStop::TimedOut, CollectionStop::Cancelled] {
        let mut queries = vec![query_with_previous_compilation()];
        let mut coverage = Coverage::default();
        let signals = collect_query_compilations(
            Reader::new(FakeRead::new(vec![Err(ReadError::Stopped(stop))])),
            60,
            &mut queries,
            &compilation_identities(),
            &mut coverage,
        )
        .await;
        assert!(signals.is_empty());
        assert_compilation_withheld(&queries[0]);
        assert_eq!(coverage.outcomes()[0].state, SourceState::Unavailable);
    }
}

#[tokio::test]
async fn duplicate_or_unrequested_native_compilation_rows_retire_the_reader_without_more_sql() {
    for rows in
        [vec![compilation_row(), compilation_row()], vec![replace(&compilation_row(), &[("query_id", Val::Int(7))])]]
    {
        let mut reader = Reader::new(FakeRead::new(vec![Ok(rows)]));
        let error = reader.compilations(&["3".into()]).await.err().unwrap();
        assert_eq!(CollectionStop::from_error(&error), CollectionStop::SourceChanged);
        assert!(reader.read_rows("must not execute").await.is_err());
        assert_eq!(reader.queries(), 1);
        assert_eq!(reader.close().await, SourceShutdown::Quiescent);
    }
}

fn query_with_previous_compilation() -> culprit_core::QueryStat {
    let mut query = query_without_costs();
    query.costs = Some(QueryCosts {
        failed: Some(1.0),
        plans: Some(5.0),
        plan_ms: Some(10.0),
        compile_history: Some(CompileHistory { count: 5.0, ..Default::default() }),
        ..Default::default()
    });
    query
}

fn assert_compilation_withheld(query: &culprit_core::QueryStat) {
    let costs = query.costs.as_ref().unwrap();
    assert_eq!(costs.failed, Some(1.0), "independent runtime costs remain intact");
    assert!(costs.plan_ms.is_none() && costs.plans.is_none() && costs.compile_history.is_none());
}
