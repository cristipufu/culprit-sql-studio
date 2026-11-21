use super::*;
fn view() -> MssqlIndexSnapshot {
    MssqlIndexSnapshot {
        parent_kind: MssqlIndexParentKind::IndexedView,
        parent_object: MssqlObjectIdentity {
            database_id: 7,
            object_id: 101,
            created_without_offset: "2026-11-10T07:00:00.123".into(),
        },
        parent_schema: "reporting".into(),
        parent_name: "Revenue".into(),
        parent_modified_without_offset: "2026-11-10T07:01:00.123".into(),
        index_id: 1,
        name: "cx_revenue".into(),
        type_code: 1,
        native_type: "CLUSTERED".into(),
        parent_memory_optimized: None,
        unique: true,
        primary_key: false,
        disabled: false,
        columns: vec![],
        columnstore: None,
        configuration: None,
        filter: None,
        reserved_bytes: None,
        usage: MssqlIndexUsage::NotCollected,
    }
}
#[test]
fn native_parent_kind_and_table_only_bit_cannot_be_defaulted_or_swapped() {
    let original = view();
    assert!(original.is_valid());
    let restored: MssqlIndexSnapshot = serde_json::from_slice(&serde_json::to_vec(&original).unwrap()).unwrap();
    assert_eq!(restored, original);
    for cause in ["missing-bit", "missing-kind", "table-bit", "heap", "columnstore", "not-unique", "constraint"] {
        let mut raw = serde_json::to_value(&original).unwrap();
        match cause {
            "missing-bit" => {
                raw.as_object_mut().unwrap().remove("parent_memory_optimized");
            }
            "missing-kind" => {
                raw.as_object_mut().unwrap().remove("parent_kind");
            }
            "table-bit" => raw["parent_memory_optimized"] = false.into(),
            "heap" => raw["type_code"] = 0.into(),
            "columnstore" => raw["type_code"] = 5.into(),
            "not-unique" => raw["unique"] = false.into(),
            _ => raw["primary_key"] = true.into(),
        }
        assert!(serde_json::from_value::<MssqlIndexSnapshot>(raw).map_or(true, |row| !row.is_valid()), "{cause}");
    }
    let mut table = original;
    table.parent_kind = MssqlIndexParentKind::Table;
    assert!(!table.is_valid());
    table.parent_memory_optimized = Some(false);
    assert!(table.is_valid());
}
#[test]
fn mixed_index_prefix_rejects_duplicate_keys_and_changed_native_parent_even_without_names() {
    let first = view();
    let mut second = first.clone();
    second.index_id = 2;
    second.type_code = 2;
    assert!(mssql_index_rows_are_consistent([&first, &second]));
    for cause in ["same-key", "database", "create", "modify", "schema", "kind"] {
        let mut changed = second.clone();
        match cause {
            "same-key" => changed.index_id = first.index_id,
            "database" => changed.parent_object.database_id += 1,
            "create" => changed.parent_object.created_without_offset = "2026-11-10T07:00:00.124".into(),
            "modify" => changed.parent_modified_without_offset = "2026-11-10T07:01:00.124".into(),
            "schema" => changed.parent_schema = "other".into(),
            _ => {
                changed.parent_kind = MssqlIndexParentKind::Table;
                changed.parent_memory_optimized = Some(false);
            }
        }
        assert!(!mssql_index_rows_are_consistent([&first, &changed]), "{cause}");
    }
}
#[test]
fn empty_partial_denied_and_combined_parent_prefix_counts_remain_distinct() {
    let source = |state| crate::SourceOutcome {
        source: crate::source_id::INDEXES.into(),
        state,
        reason: None,
        reason_message: None,
    };
    let catalog = MssqlIndexedViewCatalog {
        source: source(crate::SourceState::Collected),
        retained_index_rows: Some(3),
        parent_prefix_complete: true,
        indexes: vec![view()],
    };
    assert!(catalog.is_valid());
    let mut partial = catalog.clone();
    partial.source.state = crate::SourceState::Partial;
    partial.parent_prefix_complete = false;
    assert!(partial.is_valid());
    let mut empty = partial.clone();
    empty.indexes.clear();
    empty.retained_index_rows = Some(0);
    empty.source.state = crate::SourceState::Empty;
    assert!(!empty.is_valid());
    empty.parent_prefix_complete = true;
    assert!(empty.is_valid());
    let mut denied = empty;
    denied.source.state = crate::SourceState::Unavailable;
    denied.parent_prefix_complete = false;
    denied.retained_index_rows = None;
    assert!(denied.is_valid());
    let mut missing = catalog;
    missing.retained_index_rows = None;
    assert!(!missing.is_valid());
}
