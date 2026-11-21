use super::*;

fn fragment() -> WaitMeasurement {
    WaitMeasurement {
        key: NativeMeasurementKey {
            native: NativeIdentity::new("native-wait-fragment"),
            references: StatementReferences::default(),
            cohort: None,
        },
        category: "3".into(),
        unit: RuntimeMetricUnit::Milliseconds,
        total: MeasurementValue::Integer("9007199254740993".into()),
        average: Some(1.25),
        minimum: Some(MeasurementValue::Integer("0".into())),
        maximum: Some(MeasurementValue::Integer("9007199254740993".into())),
        last: Some(MeasurementValue::Integer("9007199254740993".into())),
        standard_deviation: Some(2.5),
        category_description: Some("Lock".into()),
        outcome_description: Some("Aborted".into()),
    }
}

#[test]
fn original_wait_fragment_roundtrip_keeps_wide_last_and_native_labels_without_changing_identity() {
    let original = fragment();
    assert!(original.is_valid());
    let encoded = serde_json::to_value(&original).unwrap();
    assert_eq!(encoded["last"]["value"], "9007199254740993");
    assert_eq!(serde_json::from_value::<WaitMeasurement>(encoded).unwrap(), original);
    let mut labels = original.clone();
    labels.category_description = Some("Future native category".into());
    labels.outcome_description = Some("Future native outcome".into());
    assert_eq!(labels.key, original.key);
    assert_eq!(labels.category, original.category);
}

#[test]
fn native_null_is_distinct_from_an_omitted_wait_fragment_column() {
    let mut value = fragment();
    value.last = None;
    value.standard_deviation = None;
    value.category_description = None;
    value.outcome_description = None;
    assert!(value.is_valid());
    let encoded = serde_json::to_value(&value).unwrap();
    assert_eq!(serde_json::from_value::<WaitMeasurement>(encoded.clone()).unwrap(), value);
    for key in ["last", "standard_deviation", "category_description", "outcome_description"] {
        assert!(encoded[key].is_null());
        let mut omitted = encoded.clone();
        omitted.as_object_mut().unwrap().remove(key);
        assert!(serde_json::from_value::<WaitMeasurement>(omitted).is_err(), "{key}");
    }
}

#[test]
fn malformed_wait_fragment_statistics_are_refused_without_rounding_or_label_reclassification() {
    for value in [f64::NAN, f64::INFINITY, -0.5] {
        let mut row = fragment();
        row.standard_deviation = Some(value);
        assert!(!row.is_valid());
    }
    for value in ["-1", "1.5", "18446744073709551616"] {
        let mut row = fragment();
        row.last = Some(MeasurementValue::Integer(value.into()));
        assert!(!row.is_valid());
    }
    let mut row = fragment();
    row.category_description = Some("x".repeat(129));
    assert!(!row.is_valid());
    row.category_description = Some("😀".repeat(64));
    assert!(row.is_valid(), "native nvarchar length counts UTF-16 units");
    row.outcome_description = Some("😀".repeat(65));
    assert!(!row.is_valid());
}
