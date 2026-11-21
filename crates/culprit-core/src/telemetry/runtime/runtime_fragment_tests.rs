use super::*;
fn metric() -> RuntimeMetric {
    RuntimeMetric {
        key: RuntimeMetricKey::Duration,
        unit: RuntimeMetricUnit::Microseconds,
        average: None,
        last: Some(MeasurementValue::Integer("9007199254740993".into())),
        minimum: Some(MeasurementValue::Integer("0".into())),
        maximum: Some(MeasurementValue::Integer(i64::MAX.to_string())),
        standard_deviation: Some(0.75),
    }
}
#[test]
fn native_bigint_endpoints_survive_unknown_mean_and_json_roundtrip_without_float_conversion() {
    let value = metric();
    assert!(value.is_valid());
    let encoded = serde_json::to_value(&value).unwrap();
    assert!(encoded["average"].is_null());
    assert_eq!(encoded["last"]["value"], "9007199254740993");
    assert_eq!(serde_json::from_value::<RuntimeMetric>(encoded).unwrap(), value);
    assert_eq!(value.last.as_ref().unwrap().exact_real(), None);
    assert_eq!(value.minimum.as_ref().unwrap().exact_real(), Some(0.0));
}
#[test]
fn omitted_statistic_keys_are_not_native_nulls_and_nonfinite_or_negative_values_are_refused() {
    let encoded = serde_json::to_value(metric()).unwrap();
    for key in ["average", "last", "minimum", "maximum", "standard_deviation"] {
        let mut missing = encoded.clone();
        missing.as_object_mut().unwrap().remove(key);
        assert!(serde_json::from_value::<RuntimeMetric>(missing).is_err(), "{key}");
    }
    for number in [-1.0, f64::NAN, f64::INFINITY] {
        let mut value = metric();
        value.average = Some(number);
        assert!(!value.is_valid());
        value.average = None;
        value.standard_deviation = Some(number);
        assert!(!value.is_valid());
    }
    let mut value = metric();
    value.last = Some(MeasurementValue::Integer("-1".into()));
    assert!(!value.is_valid());
}
#[test]
fn physical_read_operations_and_pages_are_distinct_native_units_and_unknown_statistics_are_valid() {
    let mut operations =
        RuntimeMetric::average(RuntimeMetricKey::PhysicalReadOperations, RuntimeMetricUnit::Count, 2.5);
    assert!(operations.is_valid());
    operations.unit = RuntimeMetricUnit::Pages { bytes_per_page: 8192 };
    assert!(!operations.is_valid());
    let unknown = RuntimeMetric {
        key: RuntimeMetricKey::Duration,
        unit: RuntimeMetricUnit::Microseconds,
        average: None,
        last: None,
        minimum: None,
        maximum: None,
        standard_deviation: None,
    };
    assert!(unknown.is_valid());
}
