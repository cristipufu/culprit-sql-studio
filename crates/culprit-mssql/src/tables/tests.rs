use super::*;
use crate::{
    resources::test_support::{FakeRead, replace},
    value::Val,
};

pub(crate) fn metadata(visible: bool) -> Rec {
    Rec::of(&[("metadata_visibility", Val::Int(i64::from(visible)))])
}
pub(crate) fn row() -> Rec {
    Rec::of(&[
        ("database_id", Val::Int(5)),
        ("object_id", Val::Int(100)),
        ("schema_name", Val::Text("dbo".into())),
        ("table_name", Val::Text("Orders".into())),
        ("created_without_offset", Val::Text("2026-11-10T07:00:00.123".into())),
        ("modified_without_offset", Val::Text("2026-11-10T07:01:00.123".into())),
        ("row_count", Val::Text("9007199254740993".into())),
        ("reserved_bytes", Val::Text("81920".into())),
        ("data_bytes", Val::Text("65536".into())),
        ("index_bytes", Val::Text("16384".into())),
        ("in_row_bytes", Val::Text("32768".into())),
        ("lob_bytes", Val::Text("16384".into())),
        ("overflow_bytes", Val::Text("16384".into())),
        ("sizes_available", Val::Int(1)),
        ("has_primary_key", Val::Int(1)),
        ("is_heap", Val::Int(0)),
    ])
}
#[tokio::test]
async fn scan_projection_uses_the_same_exact_native_values_and_declares_limited_metadata() {
    let mut reader = FakeRead::new(vec![Ok(vec![metadata(true)]), Ok(vec![row()])]);
    let result = application(&mut reader, true).await.unwrap();
    assert!(result.complete_scope);
    assert_eq!(result.tables[0].name, "Orders");
    assert_eq!(result.tables[0].rows, Some(9_007_199_254_740_993_u64 as f64));
    assert_eq!(result.tables[0].total_bytes, Some(81920.0));
    assert_eq!(result.tables[0].in_row_bytes, Some(32768.0));
    assert_eq!(reader.calls.len(), 2);
    assert!(reader.calls[1].sql.contains("sys.dm_db_partition_stats"));
    let mut limited = FakeRead::new(vec![Ok(vec![metadata(false)]), Ok(vec![row()])]);
    assert!(!application(&mut limited, false).await.unwrap().complete_scope);
    assert!(limited.calls[1].sql.contains("au.container_id=p.hobt_id"));
    assert!(limited.calls[1].sql.contains("au.container_id=p.partition_id"));
    assert!(!limited.calls[1].sql.contains("sys.dm_db_partition_stats"));
}
#[test]
fn native_nulls_and_unavailable_storage_stay_unknown_and_no_unknown_primary_key_becomes_false() {
    let layout = Layout::new(false);
    let unavailable = replace(
        &row(),
        &[
            ("sizes_available", Val::Int(0)),
            ("reserved_bytes", Val::Null),
            ("data_bytes", Val::Null),
            ("index_bytes", Val::Null),
            ("in_row_bytes", Val::Null),
            ("lob_bytes", Val::Null),
            ("overflow_bytes", Val::Null),
            ("row_count", Val::Text("0".into())),
            ("is_heap", Val::Null),
        ],
    );
    let native = decode(&unavailable, &layout).unwrap();
    assert_eq!(native.estimated_rows.as_deref(), Some("0"));
    assert_eq!(native.size_source, MssqlTableSizeSource::Unavailable);
    assert!(native.reserved_bytes.is_none());
    let app = project(native).unwrap();
    assert_eq!(app.rows, Some(0.0));
    assert!(app.total_bytes.is_none());
    assert!(app.is_heap.is_none());
    let unknown_primary = decode(&replace(&unavailable, &[("has_primary_key", Val::Null)]), &layout).unwrap();
    assert_eq!(project(unknown_primary).err().unwrap().code, culprit_core::ErrorCode::PermissionDenied);
    assert!(decode(&replace(&row(), &[("row_count", Val::Text("-1".into()))]), &layout).is_err());
    assert!(decode(&replace(&row(), &[("sizes_available", Val::Int(2))]), &layout).is_err());
}
#[tokio::test]
async fn failed_or_oversized_table_reads_do_not_retry_or_fall_back_to_table_contents() {
    let mut reader = FakeRead::new(vec![Ok(vec![metadata(true)]), Err(ReadError::Stopped(CollectionStop::TimedOut))]);
    assert!(application(&mut reader, true).await.is_err());
    assert_eq!(reader.calls.len(), 2);
    let mut oversized = FakeRead::new(vec![Ok(vec![metadata(true)]), Ok(vec![row(); MAX_APPLICATION_TABLES + 1])]);
    assert_eq!(
        application(&mut oversized, true).await.err().unwrap().detail,
        Some(culprit_core::ErrorDetail::CollectionStopped(CollectionStop::RowLimit))
    );
    assert!(oversized.calls[1].sql.contains("SELECT TOP (@P1)"));
    assert_eq!(oversized.calls[1].limit, MAX_APPLICATION_TABLES + 1);
}
