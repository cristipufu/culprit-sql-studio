use super::*;
use culprit_collector::analytical::schema::Field;
use serde_json::json;
fn schema() -> Schema {
    Schema::new(
        LogicalSchema::new(
            "ScalarFixture",
            [
                Field::required("Counter", "/counter", ScalarType::ExactCounter),
                Field::optional("Label", "/label", ScalarType::String),
                Field::required("At", "/at", ScalarType::Timestamp),
                Field::required("Details", "/details", ScalarType::JsonContainer),
            ],
        )
        .unwrap(),
    )
    .unwrap()
}
#[test]
fn logical_nulls_exact_counters_and_nanoseconds_survive_the_native_layout() {
    let schema = schema();
    let input = json!({"counter":"18446744073709551615","label":null,"at":"2026-11-11T10:00:00.123456789+02:00","details":{"unicode":"字段","n":1}});
    let values = schema.project(&input).unwrap();
    let encoded: Value = serde_json::from_slice(&schema.encode(&values).unwrap()).unwrap();
    assert!(encoded["Label"].is_null());
    assert!(encoded.get("HasLabel").is_none());
    assert_eq!(encoded["Counter"], "18446744073709551615");
    assert_eq!(encoded["At"], "2026-11-11T08:00:00.123456789Z");
    assert_eq!(Schema::column_type(&schema.columns()[1]), "Nullable(String)");
    let returned = vec![
        json!("18446744073709551615"),
        Value::Null,
        json!(timestamp_nanos(&values[2]).unwrap().to_string()),
        encoded["Details"].clone(),
    ];
    assert_eq!(schema.returned_digest(&returned).unwrap(), projection::digest(&values).unwrap());
    let mut empty = input.clone();
    empty["label"] = json!("");
    assert_ne!(projection::digest(&values).unwrap(), projection::digest(&schema.project(&empty).unwrap()).unwrap());
    let mut altered = returned;
    altered[2] = json!((timestamp_nanos(&values[2]).unwrap() - 1).to_string());
    assert_ne!(schema.returned_digest(&altered).unwrap(), projection::digest(&values).unwrap());
}
#[test]
fn unrepresentable_dates_and_bad_returned_types_fail_instead_of_wrapping() {
    for at in ["1500-01-01T00:00:00Z", "2500-01-01T00:00:00Z", "2026-11-11T08:00:00"] {
        assert!(schema().project(&json!({"counter":"1","label":null,"at":at,"details":{}})).is_err());
    }
    assert!(schema().returned_digest(&[json!("1"), Value::Null, json!("not-a-clock"), json!("{}")]).is_err());
    assert!(schema().returned_digest(&[json!("1")]).is_err());
}
#[test]
fn every_stream_has_one_physical_schema_without_adx_presence_columns() {
    let catalog = crate::catalog::SchemaCatalog::new().unwrap();
    assert_eq!(catalog.schemas().count(), culprit_core::telemetry::TelemetryStream::ALL.len() - 1);
    for (_, schema) in catalog.schemas() {
        assert!(schema.columns().iter().all(|column| !column.name.starts_with("Has")));
        assert_eq!(schema.columns().len(), schema.verification_columns().len());
    }
    assert!(catalog.get(culprit_core::telemetry::TelemetryStream::ActivityCheckpoints).is_err());
}
