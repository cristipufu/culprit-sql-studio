use super::*;
use crate::{
    index_operations::tests::{layout_row, row},
    read::ReadError,
    resources::test_support::{FakeRead, replace},
    value::Val,
};

pub(crate) fn point_row() -> Rec {
    let native = replace(
        &row(),
        &[
            ("database_id", Val::Int(7)),
            ("object_id", Val::Int(9_007_199)),
            ("schema_name", Val::Text("dbo".into())),
            ("object_name", Val::Text("orders".into())),
            ("index_name", Val::Text("PK_orders".into())),
            ("page_latch_wait_count", Val::Int(3)),
            ("page_latch_wait_in_ms", Val::Int(9_007_199_254_740_993)),
            ("page_io_latch_wait_count", Val::Null),
        ],
    );
    let mut fields = native.columns().map(|(name, value)| (name, value.clone())).collect::<Vec<_>>();
    fields.extend([
        ("target_ordinal", Val::Int(0)),
        ("observed_at", Val::Text("2020-01-01T06:00:00Z".into())),
        ("catalog_partition_id", Val::Int(9_007_199_254_740_993)),
        ("catalog_hobt_id", Val::Int(9_007_199_254_740_994)),
        ("catalog_index_type", Val::Int(1)),
        ("object_created_without_offset", Val::Text("2019-02-01T04:00:00".into())),
        ("object_modified_without_offset", Val::Text("2019-03-01T04:00:00".into())),
    ]);
    Rec::of(&fields)
}

fn selected(index_id: u32) -> Target {
    Target { database_id: 7, object_id: 9_007_199, index_id, partition_number: 1, partition_id: 9_007_199_254_740_993 }
}

async fn layout() -> Layout {
    Layout::read(&mut FakeRead::new(vec![Ok(vec![layout_row(true, &[])])])).await.unwrap()
}

#[tokio::test]
async fn exact_partition_points_reuse_native_units_and_never_expand_to_a_database_walk() {
    let layout = layout().await;
    let targets = [selected(1)].into();
    let mut reader = FakeRead::new(vec![Ok(vec![point_row()])]);
    let points = read(&mut reader, &layout, &targets).await.unwrap();
    let point = &points[&selected(1)];
    assert_eq!(point.counters.counter(MssqlIndexCounter::PageLatchWaitMs), Some("9007199254740993"));
    assert!(point.counters.counters[&MssqlIndexCounter::PageIoLatchWaits].is_none());
    assert_eq!(point.counters.reset, culprit_core::ResetMarker::Unknown);
    assert_eq!(point.partition_id.as_deref(), Some("9007199254740993"));
    assert!(point.object.as_ref().unwrap().qualifiers.contains_key("created_without_offset"));
    assert_eq!(reader.calls.len(), 1);
    let call = &reader.calls[0];
    assert_eq!(call.limit, 2);
    assert_eq!(call.parameters.len(), 5);
    assert!(matches!(
        call.parameters.as_slice(),
        [
            Parameter::Integer(7),
            Parameter::Integer(9_007_199),
            Parameter::Integer(1),
            Parameter::Integer(1),
            Parameter::Integer(9_007_199_254_740_993)
        ]
    ));
    assert!(call.sql.contains("sys.dm_db_index_operational_stats(@P1,@P2,@P3,@P4)"));
    assert!(call.sql.contains("LEFT JOIN sys.partitions part ON part.partition_id=@P5"));
    assert!(call.sql.contains("OPTION(MAXDOP 1,LOOP JOIN)"));
    for wildcard in ["NULL,NULL", "ORDER BY", "GROUP BY", "CROSS APPLY", "OUTER APPLY", "sys.dm_exec_query_stats"] {
        assert!(!call.sql.contains(wildcard), "{wildcard}");
    }
}

#[tokio::test]
async fn wildcard_or_unbounded_inputs_are_rejected_before_native_work_and_heap_zero_is_exact() {
    let layout = layout().await;
    let bad = [
        BTreeSet::new(),
        [Target { database_id: 0, ..selected(1) }].into(),
        [Target { object_id: 0, ..selected(1) }].into(),
        [Target { partition_number: 0, ..selected(1) }].into(),
        [Target { partition_id: 0, ..selected(1) }].into(),
        [Target { partition_id: -1, ..selected(1) }].into(),
        [Target { partition_number: i32::MAX as u32 + 1, ..selected(1) }].into(),
        (1..=9).map(|partition_number| Target { partition_number, ..selected(1) }).collect(),
    ];
    for targets in bad {
        let mut reader = FakeRead::new(vec![]);
        assert!(read(&mut reader, &layout, &targets).await.is_err());
        assert!(reader.calls.is_empty());
    }
    let heap = replace(&point_row(), &[("index_id", Val::Int(0))]);
    let mut reader = FakeRead::new(vec![Ok(vec![heap])]);
    assert!(read(&mut reader, &layout, &[selected(0)].into()).await.unwrap().contains_key(&selected(0)));
    assert!(matches!(reader.calls[0].parameters[2], Parameter::Integer(0)));
}

#[tokio::test]
async fn missing_rows_stay_missing_and_foreign_duplicate_or_invalid_rows_fail() {
    let layout = layout().await;
    let targets = [selected(1)].into();
    let mut missing = FakeRead::new(vec![Ok(vec![])]);
    assert!(read(&mut missing, &layout, &targets).await.unwrap().is_empty());
    for rows in [
        vec![point_row(), point_row()],
        vec![replace(&point_row(), &[("object_id", Val::Int(128))])],
        vec![replace(&point_row(), &[("partition_number", Val::Int(2))])],
        vec![replace(&point_row(), &[("catalog_partition_id", Val::Int(9_007_199_254_740_994))])],
        vec![replace(&point_row(), &[("target_ordinal", Val::Int(1))])],
        vec![replace(&point_row(), &[("page_latch_wait_count", Val::Int(-1))])],
    ] {
        assert!(read(&mut FakeRead::new(vec![Ok(rows)]), &layout, &targets).await.is_err());
    }
    let mut cancelled = FakeRead::new(vec![Err(ReadError::Stopped(CollectionStop::Cancelled))]);
    let error = read(&mut cancelled, &layout, &targets).await.err().unwrap();
    assert_eq!(CollectionStop::from_error(&error), CollectionStop::Cancelled);
    assert_eq!(cancelled.calls.len(), 1);
}

#[tokio::test]
async fn independent_points_preserve_hidden_catalog_and_unknown_counter_columns() {
    let layout = Layout::read(&mut FakeRead::new(vec![Ok(vec![layout_row(false, &["tree_page_latch_wait_count"])])]))
        .await
        .unwrap();
    let hidden =
        replace(&point_row(), &[("object_created_without_offset", Val::Null), ("catalog_partition_id", Val::Null)]);
    let point = decode_point(&hidden, &layout).unwrap();
    assert!(point.object.is_none() && point.partition_id.is_none());
    assert!(!point.counters.counters.contains_key(&MssqlIndexCounter::TreePageLatchWaits));
    assert!(point.counters.counter(MssqlIndexCounter::PageLatchWaits).is_some());
}

#[tokio::test]
async fn a_missing_checked_partition_keeps_counters_without_substituting_a_new_partition() {
    let layout = layout().await;
    let row = replace(&point_row(), &[("catalog_partition_id", Val::Null), ("catalog_hobt_id", Val::Null)]);
    let mut reader = FakeRead::new(vec![Ok(vec![row])]);
    let points = read(&mut reader, &layout, &[selected(1)].into()).await.unwrap();
    let point = &points[&selected(1)];
    assert!(point.partition_id.is_none());
    assert_eq!(point.counters.counter(MssqlIndexCounter::PageLatchWaits), Some("3"));
    assert_eq!(point.counters.reset, culprit_core::ResetMarker::Unknown);
    let sql = &reader.calls[0].sql;
    assert!(sql.contains("LEFT JOIN sys.partitions part ON part.partition_id=@P5"));
    let (_, filter) = sql.rsplit_once("WHERE s.database_id").expect("native counter scope filter");
    assert!(!filter.contains("part.partition_id"));
}

#[tokio::test]
async fn eight_distinct_partitions_are_one_bounded_batch_of_exact_function_arguments() {
    let layout = layout().await;
    let targets = (1..=8)
        .map(|partition_number| Target {
            partition_number,
            partition_id: selected(1).partition_id + i64::from(partition_number) - 1,
            ..selected(1)
        })
        .collect();
    let rows = (0..8)
        .map(|ordinal| {
            replace(
                &point_row(),
                &[
                    ("target_ordinal", Val::Int(ordinal)),
                    ("partition_number", Val::Int(ordinal + 1)),
                    ("catalog_partition_id", Val::Int(9_007_199_254_740_993 + ordinal)),
                ],
            )
        })
        .collect();
    let mut reader = FakeRead::new(vec![Ok(rows)]);
    assert_eq!(read(&mut reader, &layout, &targets).await.unwrap().len(), 8);
    assert_eq!(reader.calls.len(), 1);
    assert_eq!(reader.calls[0].parameters.len(), 40);
    assert_eq!(reader.calls[0].limit, 9);
    assert_eq!(reader.calls[0].sql.matches("sys.dm_db_index_operational_stats(").count(), 8);
    assert_eq!(reader.calls[0].sql.matches("UNION ALL").count(), 7);
    assert!(reader.calls[0].sql.contains("(@P36,@P37,@P38,@P39)"));
    assert!(reader.calls[0].sql.contains("part.partition_id=@P40"));
}
