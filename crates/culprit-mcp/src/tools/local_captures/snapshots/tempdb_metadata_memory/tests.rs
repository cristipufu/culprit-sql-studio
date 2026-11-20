use super::*;

fn entry() -> TempdbMetadataMemoryEntry {
    TempdbMetadataMemoryEntry {
        observation_key: "private-original-row-key".into(),
        observed_at: "2020-01-01T12:00:00.5Z".into(),
        native_consumer_id: "-7".into(),
        native_type_code: "2".into(),
        native_type_name: "VARHEAP".into(),
        description: Some("Table heap".into()),
        allocated_bytes: Some("9007199254740993".into()),
        used_bytes: Some("0".into()),
        explanation: "qualified explanation".into(),
        investigation: "bounded investigation".into(),
        verify: "separate outcome verification".into(),
    }
}

#[test]
fn consumer_projection_preserves_exact_wide_values_zero_and_native_labels_without_private_original_keys() {
    let original = entry();
    let value = consumer(&original, "opaque-display-reference".into());
    assert_eq!(value["reference"], "opaque-display-reference");
    assert_eq!(value["native_consumer_id"], "-7");
    assert_eq!(value["native_type_code"], "2");
    assert_eq!(value["native_type_name"], "VARHEAP");
    assert_eq!(value["allocated_bytes"], "9007199254740993");
    assert_eq!(value["used_bytes"], "0");
    assert_eq!(value["investigation"], original.investigation);
    assert_eq!(value["verify"], original.verify);
    assert_eq!(value.as_object().unwrap().len(), 11);
    for absent in [
        "observation_key",
        "source",
        "context",
        "qualifiers",
        "identity",
        "counters",
        "properties",
        "severity",
        "cause",
        "free_bytes",
        "fix_sql",
    ] {
        assert!(value.get(absent).is_none(), "{absent}");
    }
    assert!(!value.to_string().contains("private-original-row-key"));
}

#[test]
fn unknown_and_inapplicable_usage_are_null_instead_of_an_invented_zero_and_native_text_stays_text() {
    let mut original = entry();
    original.native_type_code = "4".into();
    original.native_type_name = "PGPOOL".into();
    original.description = Some("<native>界é</native>".into());
    original.used_bytes = None;
    let value = consumer(&original, "opaque-display-reference".into());
    assert_eq!(value["used_bytes"], Value::Null);
    assert_eq!(value["allocated_bytes"], "9007199254740993");
    assert_eq!(value["description"], "<native>界é</native>");
    original.allocated_bytes = None;
    assert_eq!(consumer(&original, "opaque-display-reference".into())["allocated_bytes"], Value::Null);
}
