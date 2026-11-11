use super::*;
use serde_json::json;

#[test]
fn full_unsigned_counter_precision_survives_decimal_verification_without_float_rounding() {
    for text in ["9007199254740993", "18446744073709551615", "0"] {
        let source = ScalarType::ExactCounter.source(&json!(text), false).unwrap();
        let actual = ScalarType::ExactCounter.returned(&json!(format!("{text}.000")), false).unwrap();
        assert_eq!(source, actual);
        assert_eq!(digest(&[source]).unwrap(), digest(&[actual]).unwrap());
    }
    assert_eq!(
        ScalarType::ExactCounter.returned(&json!("9.007199254740993e15"), false).unwrap(),
        json!("9007199254740993")
    );
    for text in ["18446744073709551616", "-1", "0.1", "NaN", "Infinity", "", "1e999999", "1.", ".5"] {
        assert!(ScalarType::ExactCounter.source(&json!(text), false).is_err(), "{text}");
    }
}
#[test]
fn missing_values_are_distinct_from_zero_and_empty_native_strings_are_preserved() {
    assert_ne!(digest(&[Value::Null]).unwrap(), digest(&[json!(0)]).unwrap());
    assert!(ScalarType::ExactCounter.returned(&json!(""), true).unwrap().is_null());
    assert!(ScalarType::ExactCounter.source(&json!(""), true).is_err());
    assert!(ScalarType::ExactCounter.returned(&Value::Null, false).is_err());
    assert_eq!(ScalarType::String.source(&json!(""), false).unwrap(), json!(""));
    assert!(
        ScalarType::String.returned(&Value::Null, true).is_err(),
        "nullable strings need an explicit presence column"
    );
}
#[test]
fn timestamp_comparison_normalizes_utc_and_only_the_documented_destination_resolution() {
    let source = ScalarType::Timestamp.source(&json!("2026-11-11T10:00:00.123456789+02:00"), false).unwrap();
    let returned = ScalarType::Timestamp.returned(&json!("2026-11-11T08:00:00.1234567Z"), false).unwrap();
    assert_eq!(source, returned);
    assert!(
        ScalarType::Timestamp.source(&json!("2026-11-11T10:00:00"), false).is_err(),
        "local clocks are not UTC timestamps"
    );
}
#[test]
fn projected_changes_type_substitutions_and_json_shape_conflicts_change_the_digest() {
    assert_ne!(digest(&[json!(12.5)]).unwrap(), digest(&[json!(12.6)]).unwrap());
    assert_ne!(digest(&[json!("12.5")]).unwrap(), digest(&[json!(12.5)]).unwrap());
    assert_ne!(digest(&[json!(["a", "b"])]).unwrap(), digest(&[json!({"a":"b"})]).unwrap());
    assert_ne!(digest(&[json!(["a:b", "c"])]).unwrap(), digest(&[json!(["a", "b:c"])]).unwrap());
    assert_ne!(digest(&[json!({"a":null})]).unwrap(), digest(&[json!({})]).unwrap());
    let left: Value = serde_json::from_str(r#"{"first":2.0,"nested":[1e3,"français"]}"#).unwrap();
    let right: Value = serde_json::from_str(r#"{"nested":[1000,"français"],"first":2}"#).unwrap();
    assert_eq!(digest(&[left]).unwrap(), digest(&[right]).unwrap());
    assert_eq!(digest(&[json!(-0.0)]).unwrap(), digest(&[json!(0)]).unwrap());
}
#[test]
fn scalar_validation_rejects_bad_mappings_and_overlarge_extensions() {
    assert!(ScalarType::Bool.returned(&json!("true"), false).is_err());
    assert!(ScalarType::Long.returned(&json!(1.5), false).is_err());
    assert!(ScalarType::Real.returned(&json!("1.5"), false).is_err());
    assert!(ScalarType::JsonContainer.source(&json!({"x":"a".repeat(MAX_COLUMN_BYTES)}), false).is_err());
    assert_eq!(
        ScalarType::Real.source(&json!(2.0), false).unwrap(),
        ScalarType::Real.returned(&json!(2), false).unwrap()
    );
}
