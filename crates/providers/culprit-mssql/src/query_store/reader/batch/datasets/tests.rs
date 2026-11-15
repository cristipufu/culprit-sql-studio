use super::*;
use crate::{
    query_store::{
        replicas::tests::row as replica,
        tests::summary_row,
        window_sql::tests::{layout_row, row as window_row},
    },
    read::ReadError,
    resources::test_support::{FakeRead, replace},
    value::Val,
};

fn runtime(group: Option<i64>, query: i64, plan: i64) -> Rec {
    replace(
        &summary_row(),
        &[
            ("replica_group_id", group.map(Val::Int).unwrap_or(Val::Null)),
            ("query_id", Val::Int(query)),
            ("plan_id", Val::Int(plan)),
            ("definition_plan_id", Val::Int(plan)),
        ],
    )
}

fn extend(row: &Rec, fields: &[(&str, Val)]) -> Rec {
    let mut values = row.columns().map(|(name, value)| (name, value.clone())).collect::<Vec<_>>();
    values.extend_from_slice(fields);
    Rec::of(&values)
}

fn wait(row: &Rec, ms: i64) -> Rec {
    extend(
        row,
        &[("wait_category", Val::Int(3)), ("wait_category_desc", Val::Text("Lock".into())), ("wait_ms", Val::Int(ms))],
    )
}

fn bucket(row: &Rec, ms: f64) -> Rec {
    extend(row, &[("bucket", Val::Int(1_753_176)), ("total_ms", Val::Float(ms))])
}

fn reader(mut replies: Vec<crate::read::ReadResult<Vec<Rec>>>) -> Reader<FakeRead> {
    let layout = replace(&layout_row(), &[("replicas", Val::Int(1))]);
    let mut initial = vec![
        Ok(vec![layout]),
        Ok(vec![replica(1, 1, None), replica(7, 2, Some("read-member"))]),
        Ok(vec![window_row()]),
    ];
    initial.append(&mut replies);
    Reader::new(FakeRead::new(initial))
}

async fn start(reader: &mut Reader<FakeRead>, group: &str) {
    reader.prepare_population_batch(1).await.unwrap();
    reader.select_batch_population(Some(&NativeIdentity::new(group))).await.unwrap();
}

#[tokio::test]
async fn each_native_dataset_is_read_once_and_partitioned_without_borrowing_other_groups() {
    let first = runtime(Some(1), 3, 4);
    let second = runtime(Some(7), 8, 5);
    let later = runtime(Some(7), 9, 6);
    let mut reader = reader(vec![
        Ok(vec![first.clone(), second.clone()]),
        Ok(vec![wait(&first, 20), wait(&second, 70), wait(&later, 5)]),
        Ok(vec![Rec::of(&[("minutes", Val::Int(15))])]),
        Ok(vec![bucket(&first, 40.0), bucket(&second, 3.0)]),
        Ok(vec![bucket(&first, 40.0), bucket(&second, 3.0)]),
    ]);
    start(&mut reader, "1").await;
    assert_eq!(reader.summary(60).await.unwrap()[0].identity.query_id, "3");
    let selected = reader.waits(60, Some(&["4".into()])).await.unwrap();
    assert_eq!(waits::fold(&selected).values[0].wait_ms, 20.0);
    let (timeline, trends) =
        crate::timeline::collect(&mut reader, 60, 1.0, None, &["3".into()]).await.unwrap().unwrap();
    assert_eq!(timeline.time_ms, [40.0]);
    assert_eq!(trends["3"].time_ms, [Some(40.0)]);
    assert_eq!(reader.queries(), 8);

    reader.select_batch_population(Some(&NativeIdentity::new("7"))).await.unwrap();
    assert_eq!(reader.summary(60).await.unwrap()[0].identity.query_id, "8");
    let selected = reader.waits(60, Some(&["5".into(), "6".into()])).await.unwrap();
    assert_eq!(selected.len(), 1, "a later new plan cannot gain original-runtime attribution");
    assert_eq!(waits::fold(&selected).values[0].wait_ms, 70.0);
    let all = reader.waits(60, None).await.unwrap();
    assert_eq!(waits::fold(&all).values[0].wait_ms, 75.0, "unmatched positive waits remain group context");
    assert!(all.iter().all(|wait| wait.plan_id.is_none()));
    let (timeline, trends) =
        crate::timeline::collect(&mut reader, 60, 1.0, None, &["8".into()]).await.unwrap().unwrap();
    assert_eq!(timeline.time_ms, [3.0]);
    assert_eq!(trends["8"].time_ms, [Some(3.0)]);
    assert_eq!(reader.queries(), 8, "later groups only select frozen rows");
    assert_eq!(reader.io.calls[4].parameters.len(), 1);
    assert!(!reader.io.calls[4].sql.contains("@P2"));
    assert!(reader.io.calls[4].sql.contains("definition_plan_id"));
    assert!(reader.io.calls[7].sql.contains("IN (3,8)"), "one bounded union covers both groups' selected trends");
    assert!(reader.series_rows(60, 30, &["8".into()]).await.is_err(), "cached buckets cannot be rebound");
    assert_eq!(reader.queries(), 8);
    assert_eq!(reader.close().await, SourceShutdown::Quiescent);
    assert_eq!(reader.io.closed, 1);
}

#[tokio::test]
async fn a_summary_cannot_rebind_a_query_incarnation_or_introduce_an_unknown_group() {
    for changed_context in [false, true] {
        let first = runtime(Some(1), 3, 4);
        let second = if changed_context {
            replace(&runtime(Some(7), 3, 4), &[("context_settings_id", Val::Int(99))])
        } else {
            runtime(Some(8), 9, 5)
        };
        let mut reader = reader(vec![Ok(vec![first, second])]);
        start(&mut reader, "1").await;
        let error = reader.summary(60).await.unwrap_err();
        assert_eq!(CollectionStop::from_error(&error), CollectionStop::SourceChanged);
        assert!(reader.summary(60).await.is_err());
        assert!(reader.waits(60, None).await.is_err());
        assert_eq!(reader.queries(), 4, "changed native identity fences all cached and native work");
        assert_eq!(reader.close().await, SourceShutdown::Quiescent);
    }
}

#[tokio::test]
async fn waits_cannot_rebind_a_matching_original_plan_to_new_statement_context() {
    let original = runtime(Some(1), 3, 4);
    let changed = replace(&original, &[("context_settings_id", Val::Int(99))]);
    let mut reader = reader(vec![Ok(vec![original]), Ok(vec![wait(&changed, 20)])]);
    start(&mut reader, "1").await;
    reader.summary(60).await.unwrap();
    let error = reader.waits(60, Some(&["4".into()])).await.unwrap_err();
    assert_eq!(CollectionStop::from_error(&error), CollectionStop::SourceChanged);
    assert!(reader.waits(60, None).await.is_err());
    assert_eq!(reader.queries(), 5);
}

#[tokio::test]
async fn nonterminal_wait_failure_is_shared_without_retrying_the_same_native_dataset() {
    let original = runtime(Some(1), 3, 4);
    let mut reader = reader(vec![
        Ok(vec![original]),
        Err(ReadError::Source(culprit_core::Error::new(
            culprit_core::ErrorCode::PermissionDenied,
            "fixture wait permission",
        ))),
    ]);
    start(&mut reader, "1").await;
    reader.summary(60).await.unwrap();
    assert!(reader.waits(60, Some(&["4".into()])).await.is_err());
    reader.select_batch_population(Some(&NativeIdentity::new("7"))).await.unwrap();
    assert!(reader.waits(60, None).await.is_err());
    assert_eq!(reader.queries(), 5);
    assert_eq!(reader.close().await, SourceShutdown::Quiescent);
}

#[tokio::test]
async fn later_trend_rows_require_the_original_query_incarnation_and_known_group() {
    for unknown_group in [false, true] {
        let original = runtime(Some(1), 3, 4);
        let changed = if unknown_group {
            replace(&original, &[("replica_group_id", Val::Int(8))])
        } else {
            replace(&original, &[("context_settings_id", Val::Int(99))])
        };
        let mut reader =
            reader(vec![Ok(vec![original.clone()]), Ok(vec![bucket(&original, 4.0)]), Ok(vec![bucket(&changed, 4.0)])]);
        start(&mut reader, "1").await;
        reader.summary(60).await.unwrap();
        let error = reader.series_rows(60, 15, &["3".into()]).await.unwrap_err();
        assert_eq!(CollectionStop::from_error(&error), CollectionStop::SourceChanged);
        assert!(reader.summary(60).await.is_err());
        assert_eq!(reader.queries(), 6);
    }
}

#[tokio::test]
async fn a_failed_totals_read_does_not_start_per_query_sql_or_retry_on_the_next_group() {
    let mut reader = reader(vec![
        Ok(vec![runtime(Some(1), 3, 4)]),
        Err(ReadError::Source(culprit_core::Error::new(
            culprit_core::ErrorCode::PermissionDenied,
            "fixture timeline permission",
        ))),
    ]);
    start(&mut reader, "1").await;
    reader.summary(60).await.unwrap();
    assert!(reader.series_rows(60, 15, &["3".into()]).await.is_err());
    reader.select_batch_population(Some(&NativeIdentity::new("7"))).await.unwrap();
    assert!(reader.series_rows(60, 15, &[]).await.is_err());
    assert_eq!(reader.queries(), 5);
}

#[tokio::test]
async fn unpartitioned_batches_reuse_native_datasets_without_inventing_replica_columns() {
    let original = runtime(None, 3, 4);
    let mut reader = Reader::new(FakeRead::new(vec![
        Ok(vec![layout_row()]),
        Ok(vec![window_row()]),
        Ok(vec![original.clone()]),
        Ok(vec![wait(&original, 20)]),
        Ok(vec![bucket(&original, 4.0)]),
        Ok(vec![bucket(&original, 4.0)]),
    ]));
    reader.prepare_population_batch(1).await.unwrap();
    reader.select_batch_population(None).await.unwrap();
    reader.summary(60).await.unwrap();
    assert_eq!(reader.waits(60, None).await.unwrap()[0].total_ms, 20);
    let (totals, queries) = reader.series_rows(60, 15, &["3".into()]).await.unwrap();
    assert_eq!(totals.len(), 1);
    assert_eq!(queries.len(), 1);
    assert_eq!(reader.queries(), 6);
    for call in reader.io.calls.iter().skip(2) {
        assert!(!call.sql.contains("ws.replica_group_id") && !call.sql.contains("rs.replica_group_id"));
    }
    assert!(reader.io.calls[5].sql.contains("query_initial_compile_at"));
}

#[tokio::test]
async fn empty_single_population_totals_do_not_start_a_per_query_read() {
    let mut reader = Reader::new(FakeRead::new(vec![
        Ok(vec![layout_row()]),
        Ok(vec![window_row()]),
        Ok(vec![runtime(None, 3, 4)]),
        Ok(vec![Rec::of(&[("minutes", Val::Int(15))])]),
        Ok(vec![]),
    ]));
    reader.summary(60).await.unwrap();
    let result = crate::timeline::collect(&mut reader, 60, 1.0, None, &["3".into()]).await.unwrap();
    assert!(result.is_none());
    assert_eq!(reader.queries(), 5, "an empty total dataset ends the timeline read");
    assert_eq!(reader.close().await, SourceShutdown::Quiescent);
}

#[tokio::test]
async fn empty_all_group_totals_are_frozen_without_any_per_query_read() {
    let mut reader = reader(vec![
        Ok(vec![runtime(Some(1), 3, 4), runtime(Some(7), 8, 5)]),
        Ok(vec![Rec::of(&[("minutes", Val::Int(15))])]),
        Ok(vec![]),
    ]);
    start(&mut reader, "1").await;
    reader.summary(60).await.unwrap();
    assert!(crate::timeline::collect(&mut reader, 60, 1.0, None, &["3".into()]).await.unwrap().is_none());
    reader.select_batch_population(Some(&NativeIdentity::new("7"))).await.unwrap();
    assert!(crate::timeline::collect(&mut reader, 60, 1.0, None, &["8".into()]).await.unwrap().is_none());
    assert_eq!(reader.queries(), 6, "all groups share the explicit empty timeline dataset");
    assert_eq!(reader.close().await, SourceShutdown::Quiescent);
}
