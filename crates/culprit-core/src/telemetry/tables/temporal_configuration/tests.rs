use super::*;
use serde_json::json;
fn original() -> MssqlTableTemporalConfiguration {
    MssqlTableTemporalConfiguration {
        temporal_type_availability: MssqlCatalogAvailability::Available,
        temporal_type: Some(2),
        temporal_type_desc_availability: MssqlCatalogAvailability::Available,
        temporal_type_desc: Some("SYSTEM_VERSIONED_TEMPORAL_TABLE".into()),
        history_table_id_availability: MssqlCatalogAvailability::Available,
        history_table_id: Some(102),
        history_retention_period_availability: MssqlCatalogAvailability::Available,
        history_retention_period: Some(6),
        history_retention_period_unit_availability: MssqlCatalogAvailability::Available,
        history_retention_period_unit: Some(5),
        history_retention_period_unit_desc_availability: MssqlCatalogAvailability::Available,
        history_retention_period_unit_desc: Some("MONTH".into()),
    }
}
#[test]
fn persisted_native_codes_ids_and_retention_round_trip_without_deleted_row_or_parent_evidence() {
    let value = original();
    assert!(value.is_valid());
    let saved = serde_json::to_value(&value).unwrap();
    let restored: MssqlTableTemporalConfiguration = serde_json::from_value(saved).unwrap();
    assert_eq!(restored, value);
    let mut signed_native = value.clone();
    signed_native.history_retention_period = Some(i32::MIN);
    assert!(signed_native.is_valid());
    let mut infinite = value.clone();
    infinite.history_retention_period = Some(-1);
    infinite.history_retention_period_unit = Some(-1);
    infinite.history_retention_period_unit_desc = Some("INFINITE".into());
    assert!(infinite.is_valid());
    // A ledger can supply history_table_id without declaring a system-versioned temporal table.
    let mut other = value;
    other.temporal_type = Some(0);
    other.temporal_type_desc = Some("NON_TEMPORAL_TABLE".into());
    assert!(other.is_valid());
}
#[test]
fn native_nulls_and_independently_unavailable_columns_are_not_synthesized_as_ordinary_or_infinite() {
    let mut saved = serde_json::to_value(original()).unwrap();
    for name in [
        "temporal_type",
        "temporal_type_desc",
        "history_table_id",
        "history_retention_period",
        "history_retention_period_unit",
        "history_retention_period_unit_desc",
    ] {
        saved[name] = json!(null);
    }
    let value: MssqlTableTemporalConfiguration = serde_json::from_value(saved.clone()).unwrap();
    assert!(value.is_valid());
    assert!(value.temporal_type.is_none());
    for state in ["unsupported", "permission-denied", "unknown"] {
        saved["temporal_type_availability"] = json!(state);
        let value: MssqlTableTemporalConfiguration = serde_json::from_value(saved.clone()).unwrap();
        assert!(value.is_valid());
        saved["temporal_type"] = json!(2);
        let value: MssqlTableTemporalConfiguration = serde_json::from_value(saved.clone()).unwrap();
        assert!(!value.is_valid());
        saved["temporal_type"] = json!(null);
    }
}
#[test]
fn contradictory_codes_and_native_identity_ranges_are_refused_but_not_repaired() {
    for (field, value) in [
        ("temporal_type", json!(3)),
        ("temporal_type_desc", json!("HISTORY_TABLE")),
        ("history_table_id", json!(0)),
        ("history_table_id", json!(2147483648u64)),
        ("history_retention_period_unit", json!(7)),
        ("history_retention_period_unit_desc", json!("DAY")),
    ] {
        let mut saved = serde_json::to_value(original()).unwrap();
        saved[field] = value;
        let invalid: MssqlTableTemporalConfiguration = serde_json::from_value(saved).unwrap();
        assert!(!invalid.is_valid(), "{field}");
    }
}
#[test]
fn missing_or_coerced_native_fields_are_not_clean_install_configuration() {
    let original = serde_json::to_value(original()).unwrap();
    for field in original.as_object().unwrap().keys() {
        let mut missing = original.clone();
        missing.as_object_mut().unwrap().remove(field);
        assert!(serde_json::from_value::<MssqlTableTemporalConfiguration>(missing).is_err(), "{field}");
    }
    for (field, value) in [
        ("temporal_type", json!("2")),
        ("history_retention_period", json!(1.5)),
        ("history_retention_period", json!(2147483648u64)),
        ("history_table_id", json!(-1)),
        ("temporal_type_availability", json!("guessed")),
    ] {
        let mut malformed = original.clone();
        malformed[field] = value;
        assert!(serde_json::from_value::<MssqlTableTemporalConfiguration>(malformed).is_err(), "{field}");
    }
}
