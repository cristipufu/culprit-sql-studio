use super::*;
use crate::{
    resources::test_support::{FakeRead, replace},
    value::Val,
};
pub(crate) fn selection() -> MssqlObjectIdentity {
    MssqlObjectIdentity { database_id: 7, object_id: 100, created_without_offset: "2020-01-01T00:00:00.0000000".into() }
}
pub(crate) fn target_row() -> Rec {
    Rec::of(&[
        ("database_id", Val::Int(7)),
        ("object_id", Val::Int(100)),
        ("created_without_offset", Val::Text("2020-01-01T00:00:00.000".into())),
        ("modified_without_offset", Val::Text("2020-01-01T00:00:00.000".into())),
        ("observed_at", Val::Text("2020-01-01T06:00:00.2Z".into())),
        ("updateability", Val::Text("READ_WRITE".into())),
        ("schema_name", Val::Text("dbo".into())),
        ("table_name", Val::Text("Hot".into())),
        ("is_memory_optimized", Val::Int(1)),
        ("xtp_supported", Val::Int(1)),
        ("dmv_permission", Val::Int(1)),
        ("table_visibility", Val::Int(1)),
        ("layout_available", Val::Int(1)),
        ("row_present", Val::Int(1)),
        ("memory_allocated_for_table_kb", Val::Int(9007199254740993)),
        ("memory_used_by_table_kb", Val::Int(0)),
        ("memory_allocated_for_indexes_kb", Val::Null),
        ("memory_used_by_indexes_kb", Val::Int(4)),
    ])
}
pub(crate) fn row() -> Rec {
    replace(&target_row(), &[("observed_at", Val::Text("2020-01-01T06:00:00.4Z".into()))])
}
#[test]
fn one_selected_metadata_relation_preserves_exact_ids_native_units_and_fixed_layout_witness() {
    for memory in [false, true] {
        let sql = query(permission(16), memory);
        assert!(sql.contains("SELECT TOP(2)"));
        assert!(sql.contains("t.object_id=@P1 AND t.is_ms_shipped=0"));
        assert!(sql.contains("VIEW DATABASE PERFORMANCE STATE"));
        assert!(sql.contains("system_type_id=127"));
        assert!(sql.contains("@observed_at AS observed_at,@updateability AS updateability"));
        assert_eq!(sql.matches("sys.dm_db_xtp_table_memory_stats m").count(), usize::from(memory));
        for forbidden in [
            "hash_index_stats",
            "memory_consumers",
            "allocation_units",
            "SELECT *",
            "COUNT_BIG",
            "ORDER BY",
            "ALTER ",
            "GRANT ",
        ] {
            assert!(!sql.contains(forbidden), "{forbidden}");
        }
    }
    assert_eq!(permission(15), "VIEW DATABASE STATE");
    assert_eq!(permission(16), "VIEW DATABASE PERFORMANCE STATE");
}
#[test]
fn native_catalog_time_normalization_preserves_ticks_without_tolerance() {
    for (raw, canonical, changed) in [
        ("2020-01-01T00:00:00.123", "2020-01-01T00:00:00.1230000", "2020-01-01T00:00:00.1230001"),
        ("2020-01-01T00:00:00", "2020-01-01T00:00:00.0000000", "2020-01-01T00:00:00.0000001"),
    ] {
        let mut target = selection();
        target.created_without_offset = canonical.into();
        let row = replace(&target_row(), &[("created_without_offset", Val::Text(raw.into()))]);
        assert!(decode_target(&row, &target).is_ok());
        assert!(
            decode_target(&replace(&row, &[("created_without_offset", Val::Text(changed.into()))]), &target).is_err()
        );
        for malformed in ["unknown", "2020-01-01T00:00:00Z", "2020-01-01T00:00:00.12300001"] {
            assert!(
                decode_target(&replace(&row, &[("created_without_offset", Val::Text(malformed.into()))]), &target)
                    .is_err()
            );
        }
    }
}
#[tokio::test]
async fn native_wide_zero_and_each_null_are_independent_gauges_with_no_empty_row_count() {
    let original = decode_target(&target_row(), &selection()).unwrap();
    for column in [
        None,
        Some("memory_allocated_for_table_kb"),
        Some("memory_used_by_table_kb"),
        Some("memory_allocated_for_indexes_kb"),
        Some("memory_used_by_indexes_kb"),
    ] {
        let row = column.map(|column| replace(&row(), &[(column, Val::Null)])).unwrap_or_else(row);
        let mut reader = FakeRead::new(vec![Ok(vec![row])]);
        let result = read(&mut reader, &selection(), &original, permission(16)).await.unwrap();
        let point = result.point.unwrap();
        assert!(point.is_valid());
        if column.is_none() {
            assert_eq!(point.allocated_table_kibibytes.as_deref(), Some("9007199254740993"));
            assert_eq!(point.used_table_kibibytes.as_deref(), Some("0"));
            assert_eq!(point.allocated_indexes_kibibytes, None);
            assert_eq!(point.used_indexes_kibibytes.as_deref(), Some("4"));
        }
    }
}
#[tokio::test]
async fn missing_native_memory_row_is_unknown_and_inconsistent_or_negative_populations_refuse() {
    let original = decode_target(&target_row(), &selection()).unwrap();
    let absent = replace(
        &row(),
        &[
            ("row_present", Val::Int(0)),
            ("memory_allocated_for_table_kb", Val::Null),
            ("memory_used_by_table_kb", Val::Null),
            ("memory_allocated_for_indexes_kb", Val::Null),
            ("memory_used_by_indexes_kb", Val::Null),
        ],
    );
    let mut reader = FakeRead::new(vec![Ok(vec![absent.clone()])]);
    assert!(read(&mut reader, &selection(), &original, permission(16)).await.unwrap().point.is_none());
    for data in [
        vec![replace(&absent, &[("memory_used_by_table_kb", Val::Int(1))])],
        vec![replace(&row(), &[("row_present", Val::Null)])],
        vec![replace(&row(), &[("memory_used_by_table_kb", Val::Int(-1))])],
        vec![row(), row()],
        vec![],
    ] {
        let mut reader = FakeRead::new(vec![Ok(data)]);
        assert!(read(&mut reader, &selection(), &original, permission(16)).await.is_err());
    }
}
#[tokio::test]
async fn same_batch_recreated_object_role_permission_layout_or_clock_changes_cannot_gain_original_scope() {
    let original = decode_target(&target_row(), &selection()).unwrap();
    for (name, value) in [
        ("database_id", Val::Int(8)),
        ("object_id", Val::Int(101)),
        ("created_without_offset", Val::Text("2020-01-02T00:00:00.000".into())),
        ("modified_without_offset", Val::Text("2020-01-02T00:00:00.000".into())),
        ("updateability", Val::Text("READ_ONLY".into())),
        ("updateability", Val::Null),
        ("dmv_permission", Val::Int(0)),
        ("dmv_permission", Val::Null),
        ("table_visibility", Val::Int(0)),
        ("layout_available", Val::Int(0)),
        ("xtp_supported", Val::Null),
        ("is_memory_optimized", Val::Int(0)),
        ("observed_at", Val::Text("2020-01-01T06:00:00.1Z".into())),
    ] {
        let mut reader = FakeRead::new(vec![Ok(vec![replace(&row(), &[(name, value)])])]);
        assert!(read(&mut reader, &selection(), &original, permission(16)).await.is_err(), "{name}");
    }
}
