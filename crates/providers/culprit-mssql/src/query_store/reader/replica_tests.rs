use super::*;
use crate::{
    query_store::{
        replicas::{Population, tests::row as replica},
        tests::summary_row,
        window_sql::tests::{layout_row, row as window_row},
    },
    read::{NativeRead, ReadError},
    resources::test_support::{FakeRead, replace},
    value::Val,
};

fn layout() -> Rec {
    replace(
        &layout_row(),
        &[("replicas", Val::Int(1)), ("actual_state_desc", Val::Text("READ_CAPTURE_SECONDARY".into()))],
    )
}
fn groups() -> Vec<Rec> {
    vec![replica(1, 1, None), replica(7, 2, Some("secondary-native-name"))]
}
fn runtime(group: i64) -> Rec {
    replace(&summary_row(), &[("replica_group_id", Val::Int(group)), ("is_forced", Val::Null)])
}
fn wait(group: i64) -> Rec {
    Rec::of(&[
        ("replica_group_id", Val::Int(group)),
        ("plan_id", Val::Int(4)),
        ("wait_category", Val::Int(3)),
        ("wait_category_desc", Val::Text("Lock".into())),
        ("wait_ms", Val::Int(40)),
    ])
}

#[tokio::test]
async fn constructor_and_malformed_population_selection_perform_no_native_work() {
    let mut reader = Reader::new(FakeRead::new(vec![]));
    assert_eq!(reader.queries(), 0);
    for selection in [
        NativeIdentity::new("7;SELECT 1"),
        NativeIdentity::new("07"),
        NativeIdentity::new("7").qualified("other-source", "unverified"),
    ] {
        assert!(reader.select_population(Some(&selection)).is_err());
    }
    assert_eq!(reader.queries(), 0);
    assert!(reader.population_catalog().is_none());
    assert_eq!(reader.close().await, SourceShutdown::Quiescent);
}

#[tokio::test]
async fn only_a_known_unpartitioned_layout_can_omit_the_actual_native_population() {
    let mut reader =
        Reader::new(FakeRead::new(vec![Ok(vec![layout_row()]), Ok(vec![window_row()]), Ok(vec![summary_row()])]));
    reader.summary(60).await.unwrap();
    assert!(reader.selected_population().unwrap().is_none());
    let catalog = reader.population_catalog().unwrap();
    assert!(
        catalog.is_valid() && !catalog.partitioned && !catalog.requires_selection && catalog.populations.is_empty()
    );
    assert_eq!(reader.close().await, SourceShutdown::Quiescent);

    let incompatible = replace(&layout_row(), &[("actual_state_desc", Val::Text("READ_CAPTURE_SECONDARY".into()))]);
    let mut reader = Reader::new(FakeRead::new(vec![Ok(vec![incompatible])]));
    assert!(reader.summary(60).await.is_err());
    assert!(reader.population_catalog().is_none(), "secondary capture without native group columns is unknown");
    assert_eq!(reader.queries(), 1);
    assert_eq!(reader.close().await, SourceShutdown::Quiescent);
}

#[tokio::test]
async fn ambiguous_groups_retain_the_catalog_without_reading_or_pooling_runtime() {
    let mut reader = Reader::new(FakeRead::new(vec![Ok(vec![layout()]), Ok(groups())]));
    let error = reader.summary(60).await.unwrap_err();
    assert_eq!(error.code, culprit_core::ErrorCode::Unsupported);
    let catalog = reader.population_catalog().unwrap();
    assert!(catalog.is_valid() && catalog.partitioned && catalog.requires_selection);
    assert_eq!(
        catalog.populations.iter().map(|population| population.identity.id.as_str()).collect::<Vec<_>>(),
        ["1", "7"]
    );
    assert!(reader.selected_population().unwrap().is_none());
    assert!(reader.summary(60).await.is_err());
    assert!(reader.waits(60, None).await.is_err());
    assert_eq!(reader.queries(), 2);
    assert_eq!(reader.close().await, SourceShutdown::Quiescent);
}

#[tokio::test]
async fn one_selected_native_group_scopes_runtime_waits_and_trends_to_the_same_frozen_window() {
    let bucket = 438_294;
    let mut reader = Reader::new(FakeRead::new(vec![
        Ok(vec![layout()]),
        Ok(groups()),
        Ok(vec![window_row()]),
        Ok(vec![runtime(7)]),
        Ok(vec![wait(7)]),
        Ok(vec![Rec::of(&[("minutes", Val::Int(60))])]),
        Ok(vec![Rec::of(&[
            ("replica_group_id", Val::Int(7)),
            ("bucket", Val::Int(bucket)),
            ("total_ms", Val::Float(4.0)),
            ("executions", Val::Int(2)),
        ])]),
        Ok(vec![Rec::of(&[
            ("replica_group_id", Val::Int(7)),
            ("query_id", Val::Int(3)),
            ("bucket", Val::Int(bucket)),
            ("total_ms", Val::Float(4.0)),
            ("executions", Val::Int(2)),
        ])]),
        Ok(vec![window_row()]),
        Ok(groups()),
    ]));
    reader.select_population(Some(&NativeIdentity::new("7"))).unwrap();
    let summary = reader.summary(60).await.unwrap();
    assert_eq!(summary[0].population, Population::ReplicaGroup(7));
    assert_eq!(summary[0].moments.executions, 2);
    assert_eq!(summary[0].moments.duration_us, 4000.0);
    assert_eq!(reader.waits(60, Some(&["4".into()])).await.unwrap()[0].total_ms, 40);
    let (timeline, trends) =
        crate::timeline::collect(&mut reader, 60, 1.0, Some("2020-01-01T07:00:00Z"), &["3".into()])
            .await
            .unwrap()
            .unwrap();
    assert_eq!(timeline.time_ms, [4.0]);
    assert_eq!(timeline.executions, [2.0]);
    assert_eq!(trends["3"].time_ms, [Some(4.0)]);
    reader.verify_window().await.unwrap();
    assert_eq!(reader.selected_population().unwrap(), Some(NativeIdentity::new("7")));
    assert_eq!(reader.queries(), 10);
    for index in [3, 4, 6, 7] {
        let query = &reader.io.calls[index];
        let alias = if index == 4 { "ws" } else { "rs" };
        assert!(query.sql.contains(&format!("{alias}.replica_group_id=7")), "{}", query.sql);
        assert!(query.sql.contains(super::super::window_sql::MEMBERSHIP));
        assert!(!query.sql.contains("SYSDATETIMEOFFSET()"));
        assert!(matches!(&query.parameters[0], Parameter::Text(scope) if scope.contains("id=\"1\"")));
    }
    assert!(reader.select_population(Some(&NativeIdentity::new("1"))).is_err());
    assert_eq!(reader.queries(), 10, "a frozen selection cannot move to another group");
    assert_eq!(reader.close().await, SourceShutdown::Quiescent);
}

#[tokio::test]
async fn a_native_reply_from_another_group_never_gets_relabelled_or_attached() {
    for wrong_wait in [false, true] {
        let mut replies = vec![
            Ok(vec![layout()]),
            Ok(groups()),
            Ok(vec![window_row()]),
            Ok(vec![runtime(if wrong_wait { 7 } else { 1 })]),
        ];
        if wrong_wait {
            replies.push(Ok(vec![wait(1)]));
        }
        let mut reader = Reader::new(FakeRead::new(replies));
        reader.select_population(Some(&NativeIdentity::new("7"))).unwrap();
        let error = if wrong_wait {
            reader.summary(60).await.unwrap();
            reader.waits(60, Some(&["4".into()])).await.unwrap_err()
        } else {
            reader.summary(60).await.unwrap_err()
        };
        assert_eq!(CollectionStop::from_error(&error), CollectionStop::SourceChanged);
        let queries = reader.queries();
        assert!(reader.read_rows("SELECT /* culprit */ 1").await.is_err());
        assert_eq!(reader.queries(), queries);
        assert_eq!(reader.close().await, SourceShutdown::Quiescent);
    }
}

#[tokio::test]
async fn malformed_or_failed_catalog_is_unknown_and_cannot_become_unpartitioned_or_healthy() {
    for catalog in
        [Ok(vec![Rec::of(&[("replica_group_id", Val::Null)])]), Err(ReadError::Stopped(CollectionStop::QueryLimit))]
    {
        let mut reader = Reader::new(FakeRead::new(vec![Ok(vec![layout()]), catalog]));
        assert!(reader.summary(60).await.is_err());
        assert!(reader.population_catalog().is_none());
        assert!(reader.selected_population().unwrap().is_none());
        assert_eq!(reader.queries(), 2);
        assert_eq!(reader.close().await, SourceShutdown::Quiescent);
    }
}

#[tokio::test]
async fn a_group_disappearing_before_final_verification_retires_the_original_population() {
    let mut reader = Reader::new(FakeRead::new(vec![
        Ok(vec![layout()]),
        Ok(groups()),
        Ok(vec![window_row()]),
        Ok(vec![runtime(7)]),
        Ok(vec![window_row()]),
        Ok(vec![replica(1, 1, None)]),
    ]));
    reader.select_population(Some(&NativeIdentity::new("7"))).unwrap();
    reader.summary(60).await.unwrap();
    let error = reader.verify_window().await.unwrap_err();
    assert_eq!(CollectionStop::from_error(&error), CollectionStop::SourceChanged);
    assert!(reader.window(60).await.is_err());
    assert_eq!(reader.queries(), 6);
    assert_eq!(reader.close().await, SourceShutdown::Quiescent);
}

#[test]
fn period_comparisons_and_unpartitioned_layouts_use_the_same_native_population_rule() {
    let population = Population::ReplicaGroup(7);
    for sql in [
        super::super::period_sql::totals_sql("'2020-01-01'", "'2020-01-02'", "'2020-01-03'", population),
        super::super::period_sql::per_plan_sql("'2020-01-01'", "'2020-01-02'", "'2020-01-03'", population),
    ] {
        assert!(sql.contains("AND rs.replica_group_id=7 AND"));
        assert!(sql.contains("SELECT rs.replica_group_id"));
        assert!(sql.contains("GROUP BY replica_group_id"));
    }
    let coverage = super::super::period_sql::coverage_sql("'2020-01-01'", "'2020-01-02'", population).unwrap();
    assert!(coverage.contains("EXISTS (SELECT 1 FROM sys.query_store_runtime_stats rs"));
    assert!(coverage.contains("rs.runtime_stats_interval_id=i.runtime_stats_interval_id AND rs.replica_group_id=7"));
    let unpartitioned_coverage =
        super::super::period_sql::coverage_sql("'2020-01-01'", "'2020-01-02'", Population::Unpartitioned).unwrap();
    assert!(!unpartitioned_coverage.contains("sys.query_store_runtime_stats rs"));
    assert!(super::super::period_sql::coverage_sql("'2020-01-01'", "'2020-01-02'", Population::AllGroups).is_err());
    let unpartitioned = runtime::summary_sql(
        false,
        false,
        crate::query_store::page_server::Availability::NotApplicable,
        Population::Unpartitioned,
    );
    assert!(unpartitioned.contains("CAST(NULL AS bigint) AS replica_group_id"));
    assert!(!unpartitioned.contains("rs.replica_group_id"));
    assert!(unpartitioned.contains("MAX(CAST(p.is_forced_plan AS int))"));
    let replica =
        runtime::summary_sql(false, false, crate::query_store::page_server::Availability::NotApplicable, population);
    assert!(replica.contains("CAST(NULL AS int) AS is_forced"));
    assert!(!replica.contains("p.is_forced_plan"));
}
