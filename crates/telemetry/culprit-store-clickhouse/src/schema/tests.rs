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

#[tokio::test]
async fn recovered_shared_resources_preserve_subject_and_scope_in_the_clickhouse_projection() {
    use culprit_collector::{
        analytical::{
            envelope::Envelope,
            test_support::{Recovery, shared_resource},
        },
        delivery::recovery::reopen,
    };
    use culprit_core::telemetry::{SharedResourceKind, TelemetryStream};
    let catalog = crate::catalog::SchemaCatalog::new().unwrap();
    let schema = catalog.get(TelemetryStream::ResourceSamples).unwrap();
    for kind in
        [SharedResourceKind::Instance, SharedResourceKind::Pool, SharedResourceKind::Cluster, SharedResourceKind::Node]
    {
        let (source, record) = shared_resource(kind, "namespace-a");
        let recovery = Recovery::capture("clickhouse:test", catalog.digest(), source, vec![record.clone()], 2);
        let range = reopen(&recovery, &recovery.manifest.range_id).await.unwrap();
        let batch = &range.manifest().batches[0];
        let row =
            culprit_core::telemetry::ExportedRecord::new(&range.manifest().source, &range.manifest().capture, record)
                .unwrap();
        let input = Envelope::committed(&range, &batch.batch_id).unwrap().input(1, &row).unwrap();
        let values = schema.project(&input).unwrap();
        let encoded: Value = serde_json::from_slice(&schema.encode(&values).unwrap()).unwrap();
        assert_eq!(encoded["ResourceId"], range.manifest().source.target.telemetry_id().unwrap());
        assert_eq!(encoded["ResourceKind"], serde_json::to_value(kind).unwrap());
        let metrics: Value = serde_json::from_str(encoded["Metrics"].as_str().unwrap()).unwrap();
        assert_eq!(metrics[0]["describes_subject"], true);
        let returned = schema
            .columns()
            .iter()
            .zip(&values)
            .map(|(column, value)| match column.kind {
                ScalarType::Timestamp if !value.is_null() => json!(timestamp_nanos(value).unwrap().to_string()),
                _ => encoded[&column.name].clone(),
            })
            .collect::<Vec<_>>();
        assert_eq!(schema.returned_digest(&returned).unwrap(), projection::digest(&values).unwrap());
        let (other, _) = shared_resource(kind, "namespace-b");
        let resource = schema.columns().iter().position(|column| column.name == "ResourceId").unwrap();
        let mut changed = returned;
        changed[resource] = json!(other.target.telemetry_id().unwrap());
        assert_ne!(schema.returned_digest(&changed).unwrap(), projection::digest(&values).unwrap());
    }
}
