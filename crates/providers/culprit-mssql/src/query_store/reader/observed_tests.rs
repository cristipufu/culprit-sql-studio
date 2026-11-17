use super::*;
use crate::{
    query_store::{
        replicas::tests::row as replica,
        tests::summary_row,
        window_sql::tests::{layout_row, row as window_row},
    },
    read::{ReadError, ReadResult},
    resources::test_support::{FakeRead, replace},
    value::Val,
};
use culprit_core::RuntimePopulationMembership;

fn layout() -> Rec {
    replace(&layout_row(), &[("replicas", Val::Int(1))])
}
fn runtime(group: Option<i64>) -> Rec {
    replace(&summary_row(), &[("replica_group_id", group.map(Val::Int).unwrap_or(Val::Null)), ("is_forced", Val::Null)])
}
fn reader(rows: Vec<Rec>, mut after: Vec<ReadResult<Vec<Rec>>>) -> Reader<FakeRead> {
    let mut script = vec![Ok(vec![layout()]), Ok(vec![]), Ok(vec![window_row()]), Ok(rows)];
    script.append(&mut after);
    Reader::new(FakeRead::new(script))
}

#[tokio::test]
async fn empty_catalog_uses_one_exact_summary_and_keeps_native_zero_and_unknown_roles() {
    for group in [0, 1, 17] {
        let mut reader =
            reader(vec![runtime(Some(group))], vec![Ok(vec![window_row()]), Ok(vec![layout()]), Ok(vec![])]);
        let rows = reader.summary(60).await.unwrap();
        assert_eq!(rows[0].population, super::super::replicas::Population::ReplicaGroup(group));
        assert_eq!(reader.selected_population().unwrap(), Some(NativeIdentity::new(group.to_string())));
        let catalog = reader.population_catalog().unwrap();
        assert!(catalog.is_valid() && catalog.partitioned && !catalog.has_complete_membership());
        assert!(matches!(catalog.membership, RuntimePopulationMembership::ObservedWindow { .. }));
        assert!(!catalog.populations[0].native_properties.contains_key("role_history"));
        assert!(!catalog.populations[0].native_properties.contains_key("replica_name"));
        assert_eq!(reader.queries(), 4);
        reader.summary(60).await.unwrap();
        assert_eq!(reader.queries(), 4, "the group is derived from the same fully drained source rows");
        reader.verify_window().await.unwrap();
        assert_eq!(reader.queries(), 7);
        assert_eq!(reader.close().await, SourceShutdown::Quiescent);
        assert_eq!(reader.io.closed, 1);
    }
}

#[tokio::test]
async fn observed_alternatives_share_one_source_without_pooling_or_selecting_a_representative() {
    let mut reader =
        reader(vec![runtime(Some(1)), runtime(Some(7))], vec![Ok(vec![window_row()]), Ok(vec![layout()]), Ok(vec![])]);
    let catalog = reader.prepare_population_batch(1, 60).await.unwrap();
    assert!(catalog.requires_selection && !catalog.has_complete_membership());
    assert_eq!(reader.queries(), 4);
    for group in ["1", "7"] {
        reader.select_batch_population(Some(&NativeIdentity::new(group))).await.unwrap();
        let rows = reader.summary(60).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].population.identity().unwrap(), Some(NativeIdentity::new(group)));
    }
    assert_eq!(reader.queries(), 4);
    assert!(reader.select_batch_population(Some(&NativeIdentity::new("99"))).await.is_err());
    assert!(reader.summary(120).await.is_err(), "the same cache cannot be borrowed by another window");
    assert_eq!(reader.queries(), 4);
    reader.finish_population_batch().await.unwrap();
    assert_eq!(reader.close().await, SourceShutdown::Quiescent);

    let mut ambiguous = self::reader(vec![runtime(Some(1)), runtime(Some(7))], vec![]);
    assert_eq!(ambiguous.summary(60).await.unwrap_err().code, culprit_core::ErrorCode::Unsupported);
    assert_eq!(ambiguous.population_catalog().unwrap().populations.len(), 2);
    assert!(ambiguous.selected_population().unwrap().is_none());
    assert_eq!(ambiguous.queries(), 4);
    assert_eq!(ambiguous.close().await, SourceShutdown::Quiescent);
}

#[tokio::test]
async fn no_rows_null_or_negative_group_and_untrusted_window_never_become_healthy_empty() {
    for rows in [vec![], vec![runtime(None)], vec![runtime(Some(-1))]] {
        let mut reader = reader(rows, vec![]);
        assert!(reader.summary(60).await.is_err());
        assert!(reader.population_catalog().is_none());
        let calls = reader.queries();
        assert!(reader.read_rows("SELECT /* culprit */ forbidden after unqualified membership").await.is_err());
        assert_eq!(reader.queries(), calls);
        assert_eq!(reader.close().await, SourceShutdown::Quiescent);
    }
    let invalid = replace(&window_row(), &[("window_clock", Val::Text("not-a-native-clock".into()))]);
    let mut reader = Reader::new(FakeRead::new(vec![Ok(vec![layout()]), Ok(vec![]), Ok(vec![invalid])]));
    assert!(reader.summary(60).await.is_err());
    assert!(reader.population_catalog().is_none());
    assert_eq!(reader.queries(), 3, "no summary is sent without a trustworthy original native window");
    assert_eq!(reader.close().await, SourceShutdown::Quiescent);

    let no_interval = replace(
        &window_row(),
        &[
            ("native_intervals", Val::Text("<intervals/>".into())),
            ("window_start", Val::Null),
            ("window_end", Val::Null),
            ("window_hours", Val::Null),
        ],
    );
    let mut reader = Reader::new(FakeRead::new(vec![Ok(vec![layout()]), Ok(vec![]), Ok(vec![no_interval])]));
    assert!(reader.summary(60).await.is_err());
    assert_eq!(reader.queries(), 3, "no runtime query is sent without actual nonempty native window bounds");
    assert!(reader.population_catalog().is_none());
    assert_eq!(reader.close().await, SourceShutdown::Quiescent);
}

#[tokio::test]
async fn unavailable_or_oversized_catalog_cannot_take_the_successfully_empty_route() {
    for catalog in [
        Err(ReadError::Stopped(CollectionStop::TimedOut)),
        Err(ReadError::Source(culprit_core::Error::new(culprit_core::ErrorCode::PermissionDenied, "native refusal"))),
        Ok(vec![replica(1, 1, None); super::super::replicas::MAX_REPLICA_GROUP_ROWS + 1]),
        Ok(vec![Rec::of(&[("replica_group_id", Val::Null)])]),
    ] {
        let mut reader = Reader::new(FakeRead::new(vec![Ok(vec![layout()]), catalog]));
        assert!(reader.summary(60).await.is_err());
        assert!(!reader.empty_replica_catalog);
        assert!(reader.population_catalog().is_none());
        assert_eq!(reader.queries(), 2);
        assert_eq!(reader.close().await, SourceShutdown::Quiescent);
    }
}

#[tokio::test]
async fn later_catalog_layout_or_database_changes_retire_every_provisional_group() {
    for change in ["catalog", "layout", "database"] {
        let final_window = if change == "database" {
            replace(&window_row(), &[("database_name", Val::Text("another-db".into()))])
        } else {
            window_row()
        };
        let final_layout = if change == "layout" { replace(&layout(), &[("replicas", Val::Null)]) } else { layout() };
        let final_catalog = if change == "catalog" { vec![replica(1, 1, None)] } else { vec![] };
        let mut reader =
            reader(vec![runtime(Some(1))], vec![Ok(vec![final_window]), Ok(vec![final_layout]), Ok(final_catalog)]);
        reader.prepare_population_batch(1, 60).await.unwrap();
        reader.select_batch_population(Some(&NativeIdentity::new("1"))).await.unwrap();
        reader.summary(60).await.unwrap();
        assert!(reader.finish_population_batch().await.is_err(), "{change}");
        let calls = reader.queries();
        assert!(reader.summary(60).await.is_err(), "{change}");
        assert!(reader.read_rows("SELECT /* culprit */ no replacement owner").await.is_err());
        assert_eq!(reader.queries(), calls);
        assert_eq!(reader.close().await, SourceShutdown::Quiescent);
    }
}

#[tokio::test]
async fn a_later_wait_group_cannot_expand_the_original_observed_population() {
    let original = runtime(Some(1));
    let foreign = runtime(Some(7));
    let mut values = foreign.columns().map(|(key, value)| (key, value.clone())).collect::<Vec<_>>();
    values.extend([
        ("wait_category", Val::Int(3)),
        ("wait_category_desc", Val::Text("Lock".into())),
        ("wait_ms", Val::Int(20)),
    ]);
    let mut reader = reader(vec![original], vec![Ok(vec![Rec::of(&values)])]);
    reader.prepare_population_batch(1, 60).await.unwrap();
    reader.select_batch_population(Some(&NativeIdentity::new("1"))).await.unwrap();
    let error = reader.waits(60, None).await.unwrap_err();
    assert_eq!(CollectionStop::from_error(&error), CollectionStop::SourceChanged);
    assert!(reader.summaries.is_none());
    let calls = reader.queries();
    assert!(reader.summary(60).await.is_err());
    assert_eq!(reader.queries(), calls);
    assert_eq!(reader.close().await, SourceShutdown::Quiescent);
}
