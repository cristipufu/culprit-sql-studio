use super::*;
use crate::{
    resources::test_support::{FakeRead, replace},
    value::Val,
};

pub(crate) fn row() -> Rec {
    let filter = "([owner]=N'private-owner')";
    Rec::of(&[
        ("configuration_fill_factor_availability",Val::Int(0)),("configuration_fill_factor",Val::Int(0)),
        ("configuration_is_padded_availability",Val::Int(0)),("configuration_is_padded",Val::Int(0)),
        ("configuration_ignore_dup_key_availability",Val::Int(0)),("configuration_ignore_dup_key",Val::Int(0)),
        ("configuration_allow_row_locks_availability",Val::Int(0)),("configuration_allow_row_locks",Val::Int(1)),
        ("configuration_allow_page_locks_availability",Val::Int(0)),("configuration_allow_page_locks",Val::Int(1)),
        ("configuration_partition_identity_availability",Val::Int(0)),
        ("configuration_data_compression_availability",Val::Int(0)),("configuration_data_compression_desc_availability",Val::Int(0)),
        ("configuration_xml_compression_availability",Val::Int(0)),("configuration_xml_compression_desc_availability",Val::Int(0)),
        ("configuration_partitions",Val::Text("[]".into())),
        ("compression_availability",Val::Int(0)),("ordering_availability",Val::Int(0)),("clustering_availability",Val::Int(0)),("compression_delay",Val::Null),
        ("database_id",Val::Int(5)),("object_id",Val::Int(100)),
        ("created_without_offset",Val::Text("2026-11-10T07:00:00.123".into())),
        ("modified_without_offset",Val::Text("2026-11-10T07:01:00.123".into())),
        ("parent_native_type",Val::Text("U".into())),("schema_name",Val::Text("dbo".into())),("parent_name",Val::Text("Child".into())),
        ("index_id",Val::Int(2)),("index_name",Val::Text("ix_child".into())),
        ("type",Val::Int(2)),("type_desc",Val::Text("NONCLUSTERED".into())),("parent_memory_optimized",Val::Int(0)),
        ("is_unique",Val::Int(0)),("is_primary_key",Val::Int(0)),("is_disabled",Val::Int(0)),("has_filter",Val::Int(1)),
        ("filter_bytes",Val::Int(filter.encode_utf16().count() as i64*2)),("filter_value",Val::Text(filter.into())),
        ("index_columns",Val::Text(r#"[{"index_column_id":1,"column_id":3,"name":"owner\u001fname","key_ordinal":2,"descending":true,"included":false,"column_store_order_ordinal":0,"data_clustering_ordinal":0},{"index_column_id":2,"column_id":1,"name":"id","key_ordinal":1,"descending":false,"included":false,"column_store_order_ordinal":0,"data_clustering_ordinal":0},{"index_column_id":3,"column_id":2,"name":"payload","key_ordinal":0,"descending":false,"included":true,"column_store_order_ordinal":0,"data_clustering_ordinal":0}]"#.into())),
        ("reserved_bytes",Val::Text("81920".into())),("usage_row_present",Val::Int(1)),
        ("user_seeks",Val::Int(9_007_199_254_740_993)),("user_scans",Val::Int(0)),("user_lookups",Val::Int(0)),("user_updates",Val::Int(3)),
        ("last_user_seek",Val::Text("2026-11-10T07:00:10.123".into())),("last_user_scan",Val::Null),
        ("last_user_lookup",Val::Null),("last_user_update",Val::Text("2026-11-10T07:00:10.123".into())),
        ("display_last_read",Val::Text("2026-11-10T05:00:10Z".into())),
    ])
}
fn layout(with_dmvs: bool) -> Layout {
    Layout { with_dmvs, expression_limit: MAX_CATALOG_EXPRESSION_BYTES }
}
fn cold() -> Rec {
    replace(
        &row(),
        &[
            ("usage_row_present", Val::Int(0)),
            ("user_seeks", Val::Null),
            ("user_scans", Val::Null),
            ("user_lookups", Val::Null),
            ("user_updates", Val::Null),
            ("last_user_seek", Val::Null),
            ("last_user_scan", Val::Null),
            ("last_user_lookup", Val::Null),
            ("last_user_update", Val::Null),
            ("display_last_read", Val::Null),
        ],
    )
}
#[tokio::test]
async fn shared_index_projection_keeps_column_names_order_exact_counts_and_redacted_filters() {
    let raw = decode(&row(), &layout(true)).unwrap();
    let MssqlIndexUsage::Observed(usage) = &raw.snapshot.usage else { panic!("usage") };
    assert_eq!(usage.user_seeks.as_deref(), Some("9007199254740993"));
    assert_eq!(usage.reset, culprit_core::ResetMarker::Unknown);
    assert!(!serde_json::to_string(&raw.snapshot).unwrap().contains("private-owner"));
    let mut observation = OperationalObservation {
        source: crate::sources::INDEXES.name.into(),
        key: raw.snapshot.key().unwrap(),
        fact: OperationalFact::MssqlIndex(Box::new(raw.snapshot.clone())),
    };
    assert!(observation.is_valid());
    observation.key = digest(b"a different index");
    assert!(!observation.is_valid(), "an alternate row key cannot duplicate one native index in the same capture");
    let app = raw.project().unwrap();
    assert_eq!(app.columns, vec!["id", "owner\u{1f}name DESC"]);
    assert_eq!(app.include, vec!["payload"]);
    assert_eq!(app.filter.as_deref(), Some("([owner]=N'private-owner')"));
    assert_eq!(app.writes, Some(3.0));
    let mut reader = FakeRead::new(vec![Ok(vec![row()])]);
    assert!(application(&mut reader, true).await.unwrap().with_usage);
    assert!(reader.calls[0].sql.contains("FOR JSON PATH"));
}
#[test]
fn absent_native_row_unsupported_usage_and_a_measured_zero_remain_distinct() {
    let absent = decode(&cold(), &layout(true)).unwrap();
    let MssqlIndexUsage::Observed(usage) = &absent.snapshot.usage else { panic!("observation") };
    assert!(!usage.row_present && usage.user_seeks.is_none());
    assert_eq!(absent.project().unwrap().reads, Some(0.0));
    let not_requested = decode(&cold(), &layout(false)).unwrap();
    assert_eq!(not_requested.snapshot.usage, MssqlIndexUsage::NotCollected);
    assert!(not_requested.project().unwrap().reads.is_none());
    for change in [("type", Val::Int(4)), ("parent_memory_optimized", Val::Int(1))] {
        let unsupported = decode(&replace(&cold(), &[change]), &layout(true)).unwrap();
        assert_eq!(unsupported.snapshot.usage, MssqlIndexUsage::NotApplicable);
        assert!(unsupported.project().unwrap().reads.is_none());
    }
    let measured = replace(&row(), &[("user_seeks", Val::Int(0))]);
    let measured = decode(&measured, &layout(true)).unwrap();
    let MssqlIndexUsage::Observed(usage) = &measured.snapshot.usage else { panic!("observation") };
    assert!(usage.row_present);
    assert_eq!(usage.user_seeks.as_deref(), Some("0"));
}
#[test]
fn malformed_flags_counters_columns_and_unreadable_filters_never_become_defaults() {
    for change in [
        ("type", Val::Int(0)),
        ("is_unique", Val::Null),
        ("user_updates", Val::Int(-1)),
        ("user_seeks", Val::Null),
        ("index_columns", Val::Text("null".into())),
        ("last_user_seek", Val::Text("2026-11-10T07:00:00Z".into())),
    ] {
        assert!(decode(&replace(&row(), &[change]), &layout(true)).is_err());
    }
    let restricted =
        decode(&replace(&row(), &[("filter_bytes", Val::Null), ("filter_value", Val::Null)]), &layout(true)).unwrap();
    assert_eq!(restricted.project().unwrap_err().code, culprit_core::ErrorCode::PermissionDenied);
}
#[tokio::test]
async fn only_a_completed_permission_refusal_allows_catalog_fallback() {
    let denied = culprit_core::Error::new(culprit_core::ErrorCode::PermissionDenied, "fixture");
    let mut reader = FakeRead::new(vec![Err(ReadError::Source(denied)), Ok(vec![cold()])]);
    let read = application(&mut reader, true).await.unwrap();
    assert!(!read.with_usage && read.table_indexes[0].reads.is_none());
    assert_eq!(reader.calls.len(), 2);
    for stop in [CollectionStop::TimedOut, CollectionStop::CancellationUnconfirmed, CollectionStop::RowLimit] {
        let mut reader = FakeRead::new(vec![Err(ReadError::Stopped(stop))]);
        assert!(application(&mut reader, true).await.is_err());
        assert_eq!(reader.calls.len(), 1);
    }
}

pub(crate) fn columnstore_row() -> Rec {
    replace(&crate::indexes::tests::row(), &[
        ("type",Val::Int(5)),("type_desc",Val::Text("CLUSTERED COLUMNSTORE".into())),
        ("has_filter",Val::Int(0)),("filter_bytes",Val::Null),("filter_value",Val::Null),
        ("compression_delay",Val::Int(15)),("clustering_availability",Val::Int(1)),
        ("index_columns",Val::Text(serde_json::json!([
            {"index_column_id":1,"column_id":3,"name":"event_type","key_ordinal":0,"descending":false,"included":true,"column_store_order_ordinal":1,"data_clustering_ordinal":null},
            {"index_column_id":2,"column_id":1,"name":"id","key_ordinal":0,"descending":false,"included":true,"column_store_order_ordinal":0,"data_clustering_ordinal":null}
        ]).to_string())),
    ])
}

fn indexed_view_row() -> Rec {
    replace(
        &cold(),
        &[
            ("parent_native_type", Val::Text("V".into())),
            ("parent_memory_optimized", Val::Null),
            ("object_id", Val::Int(101)),
            ("parent_name", Val::Text("Revenue".into())),
            ("has_filter", Val::Int(0)),
            ("filter_bytes", Val::Null),
            ("filter_value", Val::Null),
        ],
    )
}
#[tokio::test]
async fn mixed_native_parent_rows_never_project_indexed_views_into_table_advice() {
    let mut reader = FakeRead::new(vec![Ok(vec![cold(), indexed_view_row()])]);
    let read = application(&mut reader, true).await.unwrap();
    assert_eq!(read.retained_index_rows, 2);
    assert!(read.parent_prefix_complete);
    assert_eq!(read.table_indexes.len(), 1);
    assert_eq!(read.indexed_views.len(), 1);
    let view = &read.indexed_views[0];
    assert_eq!(view.parent_kind, MssqlIndexParentKind::IndexedView);
    assert_eq!(view.parent_memory_optimized, None);
    assert_eq!(view.configuration.as_ref().unwrap().parent_kind, view.parent_kind);
    assert!(decode(&indexed_view_row(), &layout(true)).unwrap().project().is_err());
    assert_eq!(reader.calls.len(), 1);
    let sql = &reader.calls[0].sql;
    assert!(sql.contains("JOIN sys.objects o ON o.object_id=i.object_id"));
    assert!(sql.contains("LEFT JOIN sys.tables native_table ON native_table.object_id=o.object_id AND o.type=''U''"));
    assert!(sql.contains("o.type IN(''U'',''V'')") && sql.contains("ORDER BY i.object_id,i.index_id"));
    assert!(!sql.contains("sys.sql_modules") && !sql.contains("OBJECT_DEFINITION"));
}
#[test]
fn native_parent_bit_kind_constraint_and_columnstore_mismatches_refuse_instead_of_guessing_a_table() {
    for changes in [
        vec![("parent_native_type", Val::Null)],
        vec![("parent_native_type", Val::Text("P".into()))],
        vec![("parent_memory_optimized", Val::Int(0))],
        vec![("type", Val::Int(1)), ("type_desc", Val::Text("CLUSTERED".into())), ("is_unique", Val::Int(0))],
        vec![("is_primary_key", Val::Int(1))],
        vec![("type", Val::Int(5))],
    ] {
        assert!(decode(&replace(&indexed_view_row(), &changes), &layout(true)).is_err(), "{changes:?}");
    }
    assert!(decode(&replace(&cold(), &[("parent_memory_optimized", Val::Null)]), &layout(true)).is_err());
}
#[tokio::test]
async fn a_combined_parent_limit_keeps_a_partial_prefix_but_checks_the_unretained_row() {
    let rows = (0..=APPLICATION_INDEXES)
        .map(|offset| replace(&cold(), &[("object_id", Val::Int(100 + offset as i64))]))
        .collect::<Vec<_>>();
    let mut reader = FakeRead::new(vec![Ok(rows.clone())]);
    let read = application(&mut reader, false).await.unwrap();
    assert_eq!(read.retained_index_rows, APPLICATION_INDEXES);
    assert_eq!(read.table_indexes.len(), APPLICATION_INDEXES);
    assert!(!read.parent_prefix_complete);
    assert_eq!(reader.calls.len(), 1);
    let mut invalid = rows;
    *invalid.last_mut().unwrap() = replace(&indexed_view_row(), &[("parent_native_type", Val::Null)]);
    let mut reader = FakeRead::new(vec![Ok(invalid)]);
    assert!(application(&mut reader, false).await.is_err());
    assert_eq!(reader.calls.len(), 1);
}
