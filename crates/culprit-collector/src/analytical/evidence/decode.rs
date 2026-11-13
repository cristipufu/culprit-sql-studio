use super::*;
use crate::delivery::{evidence::RowReceipt, recovery::CommittedRange};
use culprit_core::{Result, telemetry::*};
use serde_json::{Map, Value, json};

pub(super) enum Decoded {
    Record(Box<ExportedRecord>),
    Unavailable(EvidenceGapReason),
}

pub(super) fn record(
    range: &CommittedRange,
    stream: TelemetryStream,
    values: &Map<String, Value>,
    expected: &RowReceipt,
) -> Result<Decoded> {
    if text(values, "BatchId")? != expected.key.batch_id
        || text(values, "RecordId")? != expected.key.record_id
        || text(values, "NativePayloadDigest")? != expected.payload_digest
    {
        return Err(invalid());
    }
    let record = match stream {
        TelemetryStream::ResourceSamples => resources(values)?,
        TelemetryStream::OperationalSamples => {
            if text(values, "DetailState")? != "inline" {
                return Ok(Decoded::Unavailable(EvidenceGapReason::DetailInRecovery));
            }
            let details = values.get("Details").ok_or_else(invalid)?;
            let fact = match serde_json::from_value::<OperationalFact>(json!({
                "kind": text(values, "FactKind")?, "data": details,
            })) {
                Ok(fact) => fact,
                Err(_) => return Ok(Decoded::Unavailable(EvidenceGapReason::NativeRepresentationRequired)),
            };
            TelemetryRecord::Operational(OperationalObservation {
                source: text(values, "NativeSource")?.into(),
                key: text(values, "FactKey")?.into(),
                fact,
            })
        }
        _ => return Err(DeliveryProblem::UnsupportedSchema.error()),
    };
    let row = ExportedRecord::new(&range.manifest().source, &range.manifest().capture, record)?;
    if RowReceipt::from_export(&row)? != *expected {
        // Projection proof was checked first. A native precision/representation unavailable in that projection
        // requires the private native payload, rather than inventing an exact row from its rounded values.
        return Ok(Decoded::Unavailable(EvidenceGapReason::NativeRepresentationRequired));
    }
    Ok(Decoded::Record(Box::new(row)))
}

fn resources(values: &Map<String, Value>) -> Result<TelemetryRecord> {
    let at = text(values, "SampleAt")?.into();
    let replica_id = match values.get("ReplicaId") {
        Some(Value::Null) => None,
        Some(Value::String(id)) => Some(id.clone()),
        _ => return Err(invalid()),
    };
    let metrics: Vec<ResourceMetricSample> =
        serde_json::from_value(values.get("Metrics").ok_or_else(invalid)?.clone()).map_err(|_| invalid())?;
    Ok(TelemetryRecord::ResourceSample(ResourceMeasurement { at, replica_id, metrics }))
}

fn text<'a>(values: &'a Map<String, Value>, key: &str) -> Result<&'a str> {
    values.get(key).and_then(Value::as_str).ok_or_else(invalid)
}
fn invalid() -> culprit_core::Error {
    DeliveryProblem::Integrity.error()
}
