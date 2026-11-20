use super::*;
use crate::{
    native::jobs::test_support::*,
    progress::test_support::{Reader, row},
};

fn catalog_with_aggregates(granular: bool) -> Vec<Row> {
    let mut result = catalog();
    let mut fields = vec![
        ("hypertable_schema", 19),
        ("hypertable_name", 19),
        ("view_schema", 19),
        ("view_name", 19),
        ("view_owner", 19),
        ("materialized_only", 16),
        ("compression_enabled", 16),
        ("materialization_hypertable_schema", 19),
        ("materialization_hypertable_name", 19),
    ];
    if granular {
        fields.push(("granular_refresh_enabled", 16));
    }
    let fields =
        fields.into_iter().map(|(name, oid)| serde_json::json!({"name": name, "oid": oid})).collect::<Vec<_>>();
    let current = replace(&result[4], "columns", Some(&serde_json::to_string(&fields).unwrap()));
    let current = replace(&current, "view_oid", Some("18004"));
    result[4] = replace(&current, "can_select", Some("t"));
    result
}
fn aggregate(granular: Option<&str>) -> Row {
    row(&[
        ("observed_at", Some(AT)),
        ("hypertable_schema", Some("public")),
        ("hypertable_name", Some("raw readings")),
        ("view_schema", Some("public")),
        ("view_name", Some("hourly readings")),
        ("view_owner", Some("aggregate_owner")),
        ("materialized_only", Some("f")),
        ("compression_enabled", Some("t")),
        ("materialization_hypertable_schema", Some("_timescaledb_internal")),
        ("materialization_hypertable_name", Some("_materialized_hypertable_2")),
        ("granular_refresh_enabled", granular),
    ])
}
fn reader_with_aggregates(rows: Vec<Row>, granular: bool) -> Reader {
    let mut io = reader();
    let catalog = catalog_with_aggregates(granular);
    io.replies[1] = Ok(catalog.clone());
    io.replies[5] = Ok(catalog);
    io.replies.insert(4, Ok(rows));
    io
}
async fn captured(io: &mut Reader) -> Result<TimescaleJobsSnapshot> {
    capture(io, "fixture.invalid", 9, &bounded_budget(&CollectionBudget::default())).await
}

#[tokio::test]
async fn original_public_configuration_uses_one_paid_select_and_preserves_unknown_optional_columns() {
    for granular in [true, false] {
        let mut io = reader_with_aggregates(vec![aggregate(granular.then_some("f"))], granular);
        let read = captured(&mut io).await.unwrap();
        assert_eq!(io.calls.len(), 7);
        assert_eq!(read.sources[2].records, 1);
        assert!(read.sources[2].complete_scope);
        let OperationalFact::TimescaleContinuousAggregate(point) = &read.observations[2].fact else { panic!() };
        assert_eq!(point.view.name, "hourly readings");
        assert_eq!(point.source_hypertable.name, "raw readings");
        assert_eq!(point.materialization_hypertable.name, "_materialized_hypertable_2");
        assert!(!point.materialized_only);
        assert!(point.compression_enabled);
        assert_eq!(point.granular_refresh_enabled, granular.then_some(false));
        let sql = &io.calls[4];
        assert!(sql.ends_with("ORDER BY c.view_schema,c.view_name LIMIT 65"));
        assert_eq!(sql.contains("c.granular_refresh_enabled::text"), granular);
        for forbidden in [
            "view_definition",
            "j.config",
            "cagg_watermark",
            "refresh_continuous_aggregate",
            "hypertable_detailed_size",
            "pg_relation_size",
            "aggregate_owner",
            "raw readings",
        ] {
            assert!(!sql.contains(forbidden), "{forbidden}");
        }
    }
}

#[tokio::test]
async fn denied_and_wrong_catalog_types_do_not_erase_job_history_or_start_an_aggregate_query() {
    for denied in [true, false] {
        let mut io = reader();
        let mut catalog = catalog_with_aggregates(true);
        catalog[4] = if denied {
            replace(&catalog[4], "can_select", Some("f"))
        } else {
            let mut fields: Vec<serde_json::Value> =
                serde_json::from_str(&catalog[4].text("columns").unwrap()).unwrap();
            fields.iter_mut().find(|field| field["name"] == "materialized_only").unwrap()["oid"] =
                serde_json::json!(25);
            replace(&catalog[4], "columns", Some(&serde_json::to_string(&fields).unwrap()))
        };
        io.replies[1] = Ok(catalog.clone());
        io.replies[5] = Ok(catalog);
        let read = captured(&mut io).await.unwrap();
        assert_eq!(io.calls.len(), 6);
        assert_eq!(read.observations.len(), 2);
        assert_eq!(
            read.sources[2].error_code,
            Some(if denied { ErrorCode::PermissionDenied } else { ErrorCode::Unsupported })
        );
        assert!(io.calls.iter().all(|sql| !sql.contains("FROM timescaledb_information.continuous_aggregates")));
    }
}

#[tokio::test]
async fn aggregate_limit_and_shared_caller_limit_are_explicit_without_an_extra_page_or_budget() {
    let mut io = reader_with_aggregates(vec![aggregate(None); 65], false);
    let read = captured(&mut io).await.unwrap();
    assert_eq!(read.sources[2].records, 64);
    assert!(!read.sources[2].complete_scope);
    assert_eq!(read.sources[2].stopped, Some(CollectionStop::RowLimit));
    assert_eq!(io.calls.len(), 7);
    let mut io = reader_with_aggregates(vec![aggregate(None)], false);
    io.replies.remove(4);
    let budget = CollectionBudget { max_snapshot_rows: 2, ..bounded_budget(&CollectionBudget::default()) };
    let read = capture(&mut io, "fixture.invalid", 9, &budget).await.unwrap();
    assert_eq!(read.sources[2].state, culprit_core::SourceState::Unavailable);
    assert_eq!(read.sources[2].stopped, Some(CollectionStop::RowLimit));
    assert_eq!(read.observations.len(), 2);
    assert_eq!(io.calls.len(), 6);
}

#[tokio::test]
async fn a_settled_denial_keeps_other_sources_but_cancellation_stops_before_final_verification() {
    let mut io = reader_with_aggregates(vec![], false);
    io.replies[4] = Err(culprit_core::Error::new(ErrorCode::PermissionDenied, "owned denied-view control"));
    let read = captured(&mut io).await.unwrap();
    assert_eq!(read.observations.len(), 2);
    assert_eq!(read.sources[2].error_code, Some(ErrorCode::PermissionDenied));
    assert_eq!(io.calls.len(), 7);
    for stop in [
        CollectionStop::Cancelled,
        CollectionStop::TimedOut,
        CollectionStop::QueryLimit,
        CollectionStop::CancellationUnconfirmed,
    ] {
        let mut io = reader_with_aggregates(vec![], false);
        io.replies[4] = Err(stop.error());
        let error = captured(&mut io).await.unwrap_err();
        assert_eq!(CollectionStop::from_error(&error), stop);
        assert_eq!(io.calls.len(), 5);
    }
}

#[tokio::test]
async fn extension_or_view_replacement_discards_provisional_configuration_and_zero_is_not_freshness() {
    let mut io = reader_with_aggregates(vec![], false);
    let read = captured(&mut io).await.unwrap();
    assert_eq!(read.sources[2].state, culprit_core::SourceState::Empty);
    assert_eq!(read.sources[2].records, 0);
    assert_eq!(read.observations.len(), 2);
    let mut io = reader_with_aggregates(vec![aggregate(None)], false);
    let mut changed = catalog_with_aggregates(false);
    changed[4] = replace(&changed[4], "view_oid", Some("28004"));
    io.replies[6] = Ok(changed);
    assert_eq!(CollectionStop::from_error(&captured(&mut io).await.unwrap_err()), CollectionStop::SourceChanged);
}
