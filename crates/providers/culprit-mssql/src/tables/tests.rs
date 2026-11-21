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
        ("is_memory_optimized", Val::Int(0)),
        ("durability", Val::Int(0)),
        ("durability_desc", Val::Text("SCHEMA_AND_DATA".into())),
        ("is_tracked_by_cdc", Val::Int(0)),
        ("temporal_type_availability", Val::Int(0)),
        ("temporal_type", Val::Int(0)),
        ("temporal_type_desc_availability", Val::Int(0)),
        ("temporal_type_desc", Val::Text("NON_TEMPORAL_TABLE".into())),
        ("history_table_id_availability", Val::Int(0)),
        ("history_table_id", Val::Null),
        ("history_retention_period_availability", Val::Int(0)),
        ("history_retention_period", Val::Null),
        ("history_retention_period_unit_availability", Val::Int(0)),
        ("history_retention_period_unit", Val::Null),
        ("history_retention_period_unit_desc_availability", Val::Int(0)),
        ("history_retention_period_unit_desc", Val::Null),
        ("has_primary_key", Val::Int(1)),
        ("is_heap", Val::Int(0)),
        ("full_text_availability", Val::Int(0)),
        ("full_text_object_id", Val::Int(100)),
        ("full_text_unique_index_id", Val::Int(1)),
        ("full_text_catalog_id", Val::Int(3)),
        ("full_text_enabled", Val::Int(0)),
        ("full_text_change_tracking", Val::Text("M".into())),
        ("full_text_last_crawl_completed", Val::Int(1)),
    ])
}
pub(crate) fn without_full_text(availability: i64) -> Rec {
    replace(
        &row(),
        &[
            ("full_text_availability", Val::Int(availability)),
            ("full_text_object_id", Val::Null),
            ("full_text_unique_index_id", Val::Null),
            ("full_text_catalog_id", Val::Null),
            ("full_text_enabled", Val::Null),
            ("full_text_change_tracking", Val::Null),
            ("full_text_last_crawl_completed", Val::Null),
        ],
    )
}

#[tokio::test]
async fn cdc_enrollment_preserves_native_nulls_and_uses_the_existing_table_read() {
    for (native, expected) in [(Val::Int(0), Some(false)), (Val::Int(1), Some(true)), (Val::Null, None)] {
        let row = replace(&without_full_text(2), &[("is_tracked_by_cdc", native)]);
        let original = decode(&row, &Layout::new(true)).unwrap();
        assert_eq!(original.tracked_by_cdc, expected);
        let mut saved = serde_json::to_value(&original).unwrap();
        saved.as_object_mut().unwrap().remove("tracked_by_cdc");
        assert!(serde_json::from_value::<MssqlTableSnapshot>(saved).is_err());
        let mut reader = FakeRead::new(vec![Ok(vec![metadata(true)]), Ok(vec![row])]);
        let tables = application(&mut reader, true).await.unwrap().tables;
        assert_eq!(tables[0].mssql_tracked_by_cdc, expected);
        assert_eq!(
            tables[0].mssql_full_text.as_ref().unwrap().availability,
            MssqlCatalogAvailability::PermissionDenied
        );
        assert_eq!(reader.calls.len(), 2, "enrollment adds no native dispatch");
        assert!(reader.calls[1].sql.contains("t.is_tracked_by_cdc"));
    }
    for malformed in [Val::Int(2), Val::Text("true".into())] {
        assert!(decode(&replace(&row(), &[("is_tracked_by_cdc", malformed)]), &Layout::new(true)).is_err());
    }
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
    let full_text = result.tables[0].mssql_full_text.as_ref().unwrap();
    assert_eq!(full_text, &decode(&row(), &Layout::new(true)).unwrap().full_text);
    let index = full_text.index.as_ref().unwrap();
    assert_eq!(index.enabled, Some(false));
    assert_eq!(index.last_crawl_completed, Some(true));
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
    assert!(oversized.calls[1].sql.contains("TOP (@P1)"));
    assert_eq!(oversized.calls[1].limit, MAX_APPLICATION_TABLES + 1);
}

#[tokio::test]
async fn unavailable_full_text_catalogs_preserve_the_same_bounded_table_projection() {
    for (native, expected) in [
        (1, MssqlCatalogAvailability::Unsupported),
        (2, MssqlCatalogAvailability::PermissionDenied),
        (3, MssqlCatalogAvailability::Unknown),
    ] {
        let mut reader = FakeRead::new(vec![Ok(vec![metadata(true)]), Ok(vec![without_full_text(native)])]);
        let result = application(&mut reader, true).await.unwrap();
        assert!(result.complete_scope);
        let table = &result.tables[0];
        assert_eq!(table.total_bytes, Some(81920.0));
        assert_eq!(table.mssql_full_text.as_ref().unwrap().availability, expected);
        assert!(table.mssql_full_text.as_ref().unwrap().index.is_none());
        assert_eq!(reader.calls.len(), 2);
        assert_eq!(reader.calls[1].limit, MAX_APPLICATION_TABLES + 1);
    }
}

fn memory_optimized_row(durability: i64, description: &str) -> Rec {
    replace(
        &without_full_text(0),
        &[
            ("is_memory_optimized", Val::Int(1)),
            ("durability", Val::Int(durability)),
            ("durability_desc", Val::Text(description.into())),
            ("row_count", Val::Null),
            ("sizes_available", Val::Int(0)),
            ("reserved_bytes", Val::Null),
            ("data_bytes", Val::Null),
            ("index_bytes", Val::Null),
            ("in_row_bytes", Val::Null),
            ("lob_bytes", Val::Null),
            ("overflow_bytes", Val::Null),
        ],
    )
}

#[tokio::test]
async fn native_table_storage_configuration_survives_the_same_scan_and_stream_decoder() {
    for (memory_optimized, durability, description, native) in [
        (false, 0, "SCHEMA_AND_DATA", row()),
        (true, 0, "SCHEMA_AND_DATA", memory_optimized_row(0, "SCHEMA_AND_DATA")),
        (true, 1, "SCHEMA_ONLY", memory_optimized_row(1, "SCHEMA_ONLY")),
    ] {
        let layout = Layout::new(true);
        let streamed = decode(&native, &layout).unwrap();
        assert_eq!(streamed.storage_configuration.memory_optimized, Some(memory_optimized));
        assert_eq!(streamed.storage_configuration.durability, Some(durability));
        assert_eq!(streamed.storage_configuration.durability_desc.as_deref(), Some(description));
        if memory_optimized {
            assert_eq!(streamed.size_source, MssqlTableSizeSource::Unavailable);
            assert!(streamed.estimated_rows.is_none());
            assert!(streamed.reserved_bytes.is_none());
        }
        let mut reader = FakeRead::new(vec![Ok(vec![metadata(true)]), Ok(vec![native])]);
        let mut scan = application(&mut reader, true).await.unwrap();
        assert_eq!(scan.tables.remove(0).mssql_storage_configuration, Some(streamed.storage_configuration));
        assert_eq!(reader.calls.len(), 2);
        let sql = &reader.calls[1].sql;
        assert!(sql.contains("t.is_memory_optimized,t.durability,t.durability_desc"));
        assert!(sql.contains("TOP (@P1)"));
        assert!(!sql.contains("dm_db_xtp"));
    }
}

#[test]
fn table_configuration_native_nulls_are_independent_and_malformed_or_contradictory_facts_are_refused() {
    let layout = Layout::new(true);
    let unknown = replace(
        &row(),
        &[("is_memory_optimized", Val::Null), ("durability", Val::Null), ("durability_desc", Val::Null)],
    );
    let value = decode(&unknown, &layout).unwrap();
    assert_eq!(
        value.storage_configuration,
        MssqlTableStorageConfiguration { memory_optimized: None, durability: None, durability_desc: None }
    );
    assert_eq!(value.reserved_bytes.as_deref(), Some("81920"));
    for changes in [
        vec![("is_memory_optimized", Val::Int(2))],
        vec![("durability", Val::Int(-1))],
        vec![("durability", Val::Int(2))],
        vec![("durability", Val::Int(256))],
        vec![("durability", Val::Text("0".into()))],
        vec![("durability_desc", Val::Text("SCHEMA_ONLY".into()))],
        vec![("durability_desc", Val::Text("unexpected".into()))],
        vec![("durability", Val::Int(1)), ("durability_desc", Val::Text("SCHEMA_ONLY".into()))],
        vec![("is_memory_optimized", Val::Int(1))],
    ] {
        assert!(decode(&replace(&row(), &changes), &layout).is_err());
    }
    let missing = Rec::of(&[("durability", Val::Null), ("durability_desc", Val::Null)]);
    assert!(storage_configuration(&missing).is_err(), "absent native fields are not SQL NULLs");
}
