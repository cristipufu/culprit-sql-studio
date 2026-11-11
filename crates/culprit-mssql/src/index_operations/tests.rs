use super::*;
use crate::{
    resources::test_support::{FakeRead, replace},
    value::Val,
};
use culprit_core::telemetry::MssqlIndexCounter::*;
pub(crate) fn layout_row(sequential: bool, missing: &[&str]) -> Rec {
    let mut columns = layout::COUNTERS
        .iter()
        .map(|(_, column)| *column)
        .chain(["database_id", "object_id", "index_id", "partition_number", "hobt_id"])
        .map(|name| (name, Val::Int(i64::from(!missing.contains(&name)))))
        .collect::<Vec<_>>();
    columns.extend([("metadata_visibility", Val::Int(1)), ("sequential_option", Val::Int(i64::from(sequential)))]);
    Rec::of(&columns)
}
pub(crate) fn row() -> Rec {
    let mut columns = layout::COUNTERS.iter().map(|(_, column)| (*column, Val::Int(0))).collect::<Vec<_>>();
    columns.extend([
        ("database_id", Val::Int(5)),
        ("object_id", Val::Int(100)),
        ("index_id", Val::Int(1)),
        ("partition_number", Val::Int(1)),
        ("hobt_id", Val::Null),
        ("schema_name", Val::Text("dbo".into())),
        ("object_name", Val::Text("Orders".into())),
        ("index_name", Val::Text("ix_Orders".into())),
        ("object_type", Val::Text("U".into())),
        ("engine_internal", Val::Int(0)),
        ("leading_identity", Val::Int(0)),
        ("leading_default", Val::Text("(NEXT VALUE FOR [dbo].[ids])".into())),
        ("optimize_sequential", Val::Null),
    ]);
    Rec::of(&columns)
}
async fn layout(sequential: bool, missing: &[&str]) -> Layout {
    Layout::read(&mut FakeRead::new(vec![Ok(vec![layout_row(sequential, missing)])])).await.unwrap()
}
#[tokio::test]
async fn columns_are_selected_before_reading_and_native_errors_are_not_retried() {
    let layout = layout(false, &["leaf_update_count", "hobt_id"]).await;
    let sql = select_sql(&layout);
    assert!(!sql.contains("s.leaf_update_count"));
    assert!(!sql.contains("i.optimize_for_sequential_key"));
    assert!(sql.contains("CAST(NULL AS bigint) AS hobt_id"));
    let decoded = decode(&row(), &layout).unwrap();
    assert!(!decoded.counters.contains_key(&LeafUpdates));
    assert!(select_sql(&self::layout(true, &[]).await).contains("i.optimize_for_sequential_key"));
    let mut failed = FakeRead::new(vec![Err(ReadError::Source(invalid_data()))]);
    assert!(application(&mut failed).await.is_err());
    assert_eq!(failed.calls.len(), 1);
    let mut hidden = FakeRead::new(vec![Ok(vec![replace(
        &layout_row(false, &["database_id"]),
        &[("metadata_visibility", Val::Null)],
    )])]);
    assert_eq!(Layout::read(&mut hidden).await.err().unwrap().code, culprit_core::ErrorCode::PermissionDenied);
}
#[tokio::test]
async fn decoding_preserves_exact_counters_nulls_hidden_names_and_only_direct_defaults() {
    let layout = layout(true, &[]).await;
    let native = replace(
        &row(),
        &[
            ("leaf_insert_count", Val::Int(9_007_199_254_740_993)),
            ("row_lock_wait_count", Val::Null),
            ("schema_name", Val::Null),
            ("hobt_id", Val::Int(i64::MAX)),
        ],
    );
    let fact = decode(&native, &layout).unwrap();
    assert_eq!(fact.counter(LeafInserts), Some("9007199254740993"));
    assert_eq!(fact.counters.get(&RowLockWaits), Some(&None));
    assert_eq!(fact.counter(PageLockWaits), Some("0"));
    assert!(fact.schema.is_none());
    assert_eq!(fact.identity.hobt_id.as_deref(), Some("9223372036854775807"));
    assert_eq!(fact.leading_key_default, Some(culprit_core::sql::DefaultGenerator::Sequence));
    assert_eq!(fact.reset, culprit_core::ResetMarker::Unknown);
    for expression in ["N'GETDATE()'", "(dbo.next_value())", "(GETDATE()+1)"] {
        assert!(
            decode(&replace(&row(), &[("leading_default", Val::Text(expression.into()))]), &layout)
                .unwrap()
                .leading_key_default
                .is_none()
        );
    }
    assert!(decode(&replace(&row(), &[("leaf_insert_count", Val::Int(-1))]), &layout).is_err());
    assert_eq!(MssqlIndexCounter::RowLockWaitMs.unit(), CounterUnit::Milliseconds);
    assert_eq!(MssqlIndexCounter::LobFetchBytes.unit(), CounterUnit::Bytes);
    assert_eq!(MssqlIndexCounter::LobFetchPages.unit(), CounterUnit::Pages { bytes_per_page: 8192 });
}
#[tokio::test]
async fn partition_sums_are_exact_and_missing_partition_values_never_become_zero() {
    let layout = layout(true, &[]).await;
    let first = decode(
        &replace(
            &row(),
            &[
                ("row_lock_wait_count", Val::Int(3)),
                ("row_lock_wait_in_ms", Val::Int(7)),
                ("leaf_insert_count", Val::Int(9_007_199_254_740_993)),
                ("optimize_sequential", Val::Int(1)),
            ],
        ),
        &layout,
    )
    .unwrap();
    let mut second = first.clone();
    second.identity.partition_number = 2;
    second.counters.insert(LeafInserts, Some("1".into()));
    let summary = projection::summarize(&[first.clone(), second.clone()]).unwrap();
    let contention = summary.contention.values().next().unwrap();
    assert_eq!(contention.row_lock_waits, 6.0);
    assert_eq!(contention.row_lock_wait_ms, 14.0);
    assert_eq!(contention.leaf_inserts, Some(9_007_199_254_740_994.0));
    assert!(contention.sequential_key);
    assert_eq!(contention.optimize_for_sequential_key, Some(true));
    second.counters.insert(RowLockWaits, None);
    let partial = projection::summarize(&[first, second]).unwrap();
    assert!(partial.contention.is_empty());
    assert!(!partial.contention_complete);
    let mut heap = decode(&replace(&row(), &[("index_id", Val::Int(0)), ("index_name", Val::Null)]), &layout).unwrap();
    let summary = projection::summarize(&[heap.clone()]).unwrap();
    assert_eq!(summary.forwarded.values().next(), Some(&0.0));
    heap.schema = None;
    let partial = projection::summarize(&[heap]).unwrap();
    assert!(!partial.forwarded_complete);
    assert!(partial.forwarded.is_empty());
}

#[tokio::test]
async fn a_measured_zero_is_retained_as_a_baseline_without_inventing_a_reset_epoch() {
    let layout = layout(true, &[]).await;
    let native = decode(&row(), &layout).unwrap();
    let summary = projection::summarize(&[native]).unwrap();
    assert!(summary.contention_complete);
    assert_eq!(summary.contention.len(), 1);
    let contention = summary.contention.values().next().unwrap();
    assert_eq!(contention.lock_waits(), 0.0);
    assert_eq!(contention.lock_wait_ms(), 0.0);
    assert_eq!(contention.lock_escalations, 0.0);
    assert_eq!(contention.reset, culprit_core::ResetMarker::Unknown);
}
#[tokio::test]
async fn exact_catalog_names_keep_case_sensitive_objects_separate_and_rows_are_bounded() {
    let layout = layout(true, &[]).await;
    let first = decode(&replace(&row(), &[("row_lock_wait_count", Val::Int(1))]), &layout).unwrap();
    let mut second = first.clone();
    second.identity.object_id = 101;
    second.object = Some("orders".into());
    let summary = projection::summarize(&[first, second]).unwrap();
    assert_eq!(summary.contention.len(), 2);
    let mut reader = FakeRead::new(vec![Ok(vec![layout_row(true, &[])]), Ok(vec![row(); MAX_APPLICATION_ROWS + 1])]);
    assert_eq!(application(&mut reader).await.err().unwrap().detail, CollectionStop::RowLimit.error().detail);
    assert_eq!(reader.calls.len(), 2);
    assert_eq!(reader.calls[1].limit, MAX_APPLICATION_ROWS + 1);
}
