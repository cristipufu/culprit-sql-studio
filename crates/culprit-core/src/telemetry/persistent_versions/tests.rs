use super::*;
use serde_json::json;

fn store() -> MssqlPersistentVersionStore {
    serde_json::from_value(json!({
        "available_columns": ["persistent_version_store_size_kb"],
        "off_row_cleanup": {},
        "aborted_cleanup": {}
    }))
    .unwrap()
}

const SKIPPED_COLUMNS: [&str; 4] = [
    "pvs_off_row_page_skipped_oldest_active_xdesid",
    "pvs_off_row_page_skipped_min_useful_xts",
    "pvs_off_row_page_skipped_oldest_snapshot",
    "pvs_off_row_page_skipped_oldest_aborted_xdesid",
];

fn set_skipped(store: &mut MssqlPersistentVersionStore, position: usize, value: Option<String>) {
    match position {
        0 => store.pages_skipped_for_oldest_active_transaction = value,
        1 => store.pages_skipped_for_long_snapshot_scan = value,
        2 => store.pages_skipped_for_online_index_rebuild = value,
        3 => store.pages_skipped_for_oldest_aborted_transaction = value,
        _ => unreachable!(),
    }
}

#[test]
fn cleanup_counts_preserve_exact_zero_and_unknown_without_inventing_column_availability() {
    let absent = store();
    assert!(absent.is_valid());
    let mut present_null = absent.clone();
    present_null.available_columns.extend(SKIPPED_COLUMNS.map(str::to_owned));
    assert!(present_null.is_valid());
    assert_ne!(present_null, absent, "native NULL and an unavailable native column remain distinct");

    for (position, value) in
        ["0", "9007199254740993", "9223372036854775807", "18446744073709551615"].into_iter().enumerate()
    {
        set_skipped(&mut present_null, position, Some(value.into()));
    }
    assert!(present_null.is_valid());
    let restored: MssqlPersistentVersionStore =
        serde_json::from_slice(&serde_json::to_vec(&present_null).unwrap()).unwrap();
    assert_eq!(restored, present_null);
}

#[test]
fn unavailable_columns_and_invalid_counts_cannot_authorize_cleanup_mechanisms() {
    for (position, column) in SKIPPED_COLUMNS.into_iter().enumerate() {
        let mut value = store();
        set_skipped(&mut value, position, Some("0".into()));
        assert!(!value.is_valid(), "a value must name an available native column: {column}");
        value.available_columns.push(column.into());
        assert!(value.is_valid());
        for invalid in ["-1", "1.5", "18446744073709551616", "unknown"] {
            set_skipped(&mut value, position, Some(invalid.into()));
            assert!(!value.is_valid(), "{column}: {invalid}");
        }
    }

    let mut missing_sweep_column = store();
    missing_sweep_column.off_row_cleanup.started_without_offset = Some("2020-01-01T07:58:30.1533333".into());
    assert!(!missing_sweep_column.is_valid());
    missing_sweep_column.available_columns.push("offrow_version_cleaner_start_time".into());
    assert!(missing_sweep_column.is_valid());
}
