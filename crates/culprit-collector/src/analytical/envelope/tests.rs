use super::*;
use crate::analytical::schema::LogicalSchema;
use crate::analytical::test_support::fixture;
use culprit_core::telemetry::TelemetryRecord;

#[test]
fn envelope_keeps_collection_measurement_and_native_observation_times_distinct() {
    let (manifest, row) = fixture();
    let envelope = Envelope::new(&manifest, &row.batch_id).unwrap();
    let input = envelope.input(1, &row).unwrap();
    let schema = LogicalSchema::new("FixtureV1", COMMON.iter().copied()).unwrap();
    let projected = schema.project(&input).unwrap();
    let wire: Value = serde_json::from_slice(&schema.encode(&projected).unwrap()).unwrap();
    assert_eq!(wire["RangeId"], manifest.range_id);
    assert_eq!(wire["RecordId"], row.record_id.as_ref().unwrap().as_str());
    assert_eq!(wire["CollectedFrom"], "2020-01-01T12:00:00.000000000Z");
    assert_eq!(wire["IntervalFrom"], "2020-01-01T06:00:00.000000000Z");
    assert_eq!(wire["ObservedAt"], "2020-01-01T06:02:00.000000000Z");
    assert!(wire["ObservedUntil"].is_null());
    assert!(wire["IntervalId"].is_null());
    assert!(wire.get("HasIntervalId").is_none());
    assert_eq!(wire["OwnerEpoch"], 1);
    assert_eq!(wire["CaptureSequence"], 2);
    assert_eq!(input["record"]["data"]["at"], "2020-01-01T06:00:15.000000000Z");
}
#[test]
fn unexpected_rows_wrong_ordinals_and_native_payload_changes_are_rejected() {
    let (manifest, row) = fixture();
    let envelope = Envelope::new(&manifest, &row.batch_id).unwrap();
    assert!(envelope.input(0, &row).is_err());
    assert!(envelope.input(2, &row).is_err());
    let mut changed = row.clone();
    changed.capture.revision.as_mut().unwrap().outbox_id = "other".into();
    assert!(envelope.input(1, &changed).is_err());
    let mut changed = row.clone();
    let TelemetryRecord::ResourceSample(sample) = &mut changed.record else { unreachable!() };
    sample.metrics[0].value = Some(99.5);
    assert!(envelope.input(1, &changed).is_err());
    assert!(Envelope::new(&manifest, &"a".repeat(64)).is_err());
}
