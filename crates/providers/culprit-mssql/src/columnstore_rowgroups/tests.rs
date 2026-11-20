use super::*;
use crate::{resources::test_support::replace, value::Val};
pub(crate) fn selection() -> MssqlColumnstoreTarget {
    MssqlColumnstoreTarget {
        table: MssqlObjectIdentity {
            database_id: 7,
            object_id: 100,
            created_without_offset: "2020-01-01T00:00:00.000".into(),
        },
        index_id: 1,
        table_modified_without_offset: "2020-01-01T00:00:00.000".into(),
        partition_number: Some(1),
    }
}
pub(crate) fn target_row() -> Rec {
    Rec::of(&[
        ("database_id", Val::Int(7)),
        ("object_id", Val::Int(100)),
        ("index_id", Val::Int(1)),
        ("created_without_offset", Val::Text("2020-01-01T00:00:00.000".into())),
        ("modified_without_offset", Val::Text("2020-01-01T00:00:00.000".into())),
        ("observed_at", Val::Text("2020-01-01T06:00:00.2Z".into())),
        ("index_type", Val::Int(5)),
        ("is_memory_optimized", Val::Int(0)),
        ("can_view_definition", Val::Int(1)),
        ("partition_present", Val::Int(1)),
        ("layout_available", Val::Int(1)),
    ])
}
pub(crate) fn row() -> Rec {
    Rec::of(&[
        ("database_id", Val::Int(7)),
        ("object_id", Val::Int(100)),
        ("index_id", Val::Int(1)),
        ("partition_number", Val::Int(1)),
        ("row_group_id", Val::Int(0)),
        ("index_type", Val::Int(5)),
        ("observed_at", Val::Text("2020-01-01T06:00:00.4Z".into())),
        ("delta_store_hobt_id", Val::Null),
        ("state", Val::Int(3)),
        ("total_rows", Val::Int(2)),
        ("deleted_rows", Val::Int(0)),
        ("size_in_bytes", Val::Int(9007199254740993)),
    ])
}
#[test]
fn catalog_query_selects_only_the_exact_target_with_lookahead_and_no_physical_or_user_data_work() {
    assert!(sql::ROWGROUPS.contains("TOP(@P1)"));
    assert!(sql::ROWGROUPS.contains("rg.object_id=@P2 AND rg.index_id=@P3"));
    assert!(sql::ROWGROUPS.contains("rg.partition_number=@P4"));
    assert!(sql::TARGET.contains("VIEW DEFINITION"));
    assert!(sql::TARGET.contains("system_type_id=127"));
    for sql in [sql::TARGET, sql::ROWGROUPS] {
        for forbidden in [
            "dm_db_column_store",
            "row_group_physical",
            "dictionaries",
            "segments",
            "ORDER BY",
            "COUNT_BIG",
            "SELECT *",
            "ALTER INDEX",
            "REORGANIZE",
        ] {
            assert!(!sql.contains(forbidden), "{forbidden}");
        }
    }
}
#[test]
fn target_signature_denial_unknown_layout_hidden_partition_and_memory_tables_fail_closed() {
    assert_eq!(validate_target(&target_row(), &selection()).unwrap().index_type, 5);
    for (name, value) in [
        ("database_id", Val::Int(8)),
        ("object_id", Val::Int(101)),
        ("index_id", Val::Int(2)),
        ("modified_without_offset", Val::Text("2020-01-02T00:00:00.000".into())),
        ("can_view_definition", Val::Int(0)),
        ("can_view_definition", Val::Null),
        ("layout_available", Val::Int(0)),
        ("partition_present", Val::Int(0)),
        ("index_type", Val::Int(1)),
        ("is_memory_optimized", Val::Int(1)),
    ] {
        assert!(validate_target(&replace(&target_row(), &[(name, value)]), &selection()).is_err(), "{name}");
    }
}
#[test]
fn native_catalog_precision_preserves_exact_selected_table_creation_and_modification() {
    for (native, canonical, changed) in [
        ("2020-01-01T00:00:00.123", "2020-01-01T00:00:00.1230000", "2020-01-01T00:00:00.1230001"),
        ("2020-01-01T00:00:00", "2020-01-01T00:00:00.0000000", "2020-01-01T00:00:00.0000001"),
    ] {
        let mut target = selection();
        target.table.created_without_offset = canonical.into();
        target.table_modified_without_offset = canonical.into();
        let native_row = replace(
            &target_row(),
            &[
                ("created_without_offset", Val::Text(native.into())),
                ("modified_without_offset", Val::Text(native.into())),
            ],
        );
        let admitted = validate_target(&native_row, &target).unwrap();
        assert_eq!(admitted.index_type, 5);
        assert_eq!(admitted.observed_at, canonical_time("2020-01-01T06:00:00.2Z").unwrap());
        for column in ["created_without_offset", "modified_without_offset"] {
            let changed_row = replace(&native_row, &[(column, Val::Text(changed.into()))]);
            let error = validate_target(&changed_row, &target).unwrap_err();
            assert_eq!(error.detail, CollectionStop::SourceChanged.error().detail, "{column}: {native}");
            for malformed in ["unknown", "2020-01-01T00:00:00Z", "2020-01-01T00:00:00.12300001"] {
                let malformed_row = replace(&native_row, &[(column, Val::Text(malformed.into()))]);
                assert!(validate_target(&malformed_row, &target).is_err(), "{column}: {malformed}");
            }
        }
    }
}
#[test]
fn native_bigint_null_and_zero_are_exact_and_foreign_or_negative_native_identifiers_refuse() {
    let point = decode(&row(), &selection(), 5).unwrap();
    assert_eq!(point.size_bytes.as_deref(), Some("9007199254740993"));
    assert_eq!(point.delta_store_hobt_id, None);
    assert_eq!(point.deleted_rows.as_deref(), Some("0"));
    for (name, value) in [
        ("database_id", Val::Int(8)),
        ("object_id", Val::Int(101)),
        ("index_type", Val::Int(6)),
        ("row_group_id", Val::Int(-1)),
        ("partition_number", Val::Int(-1)),
    ] {
        assert!(decode(&replace(&row(), &[(name, value)]), &selection(), 5).is_err(), "{name}");
    }
}
