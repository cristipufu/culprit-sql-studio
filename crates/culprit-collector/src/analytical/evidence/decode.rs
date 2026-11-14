use super::*;
use crate::delivery::{evidence::RowReceipt, recovery::CommittedRange};
use culprit_core::{Result, telemetry::*};
use serde_json::{Map, Value, json};

mod payloads;
mod queries;

pub(super) enum Decoded<T> {
    Record(Box<T>),
    Unavailable(EvidenceGapReason),
}

pub(super) fn record(
    range: &CommittedRange,
    stream: TelemetryStream,
    values: &Map<String, Value>,
    expected: &RowReceipt,
) -> Result<Decoded<ExportedRecord>> {
    if text(values, "BatchId")? != expected.key.batch_id
        || text(values, "RecordId")? != expected.key.record_id
        || text(values, "NativePayloadDigest")? != expected.payload_digest
    {
        return Err(invalid());
    }
    let payloads = payloads::references(values)?;
    let native = match stream {
        TelemetryStream::ResourceSamples => Decoded::Record(Box::new(resources(values)?)),
        TelemetryStream::ActivityObservations => activity(values, &payloads)?,
        TelemetryStream::OperationalSamples => {
            if text(values, "DetailState")? != "inline" {
                return Ok(Decoded::Unavailable(EvidenceGapReason::DetailInRecovery));
            }
            let mut details = match payloads::details(values, &payloads)? {
                Decoded::Record(details) => details,
                Decoded::Unavailable(reason) => return Ok(Decoded::Unavailable(reason)),
            };
            if text(values, "FactKind")? == "diagnostic-revision" {
                let scope = details.pointer_mut("/episode/scope").and_then(Value::as_object_mut).ok_or_else(invalid)?;
                if scope.contains_key("target") {
                    return Err(invalid());
                }
                scope.insert(
                    "target".into(),
                    serde_json::to_value(&range.manifest().source.target).map_err(|_| invalid())?,
                );
            }
            let fact = match serde_json::from_value::<OperationalFact>(json!({
                "kind": text(values, "FactKind")?, "data": details,
            })) {
                Ok(fact) => fact,
                Err(_) => return Ok(Decoded::Unavailable(EvidenceGapReason::NativeRepresentationRequired)),
            };
            Decoded::Record(Box::new(TelemetryRecord::Operational(OperationalObservation {
                source: text(values, "NativeSource")?.into(),
                key: text(values, "FactKey")?.into(),
                fact,
            })))
        }
        TelemetryStream::QueryDefinitions
        | TelemetryStream::PlanDefinitions
        | TelemetryStream::QueryRuntime
        | TelemetryStream::QueryWaits => queries::record(stream, values, &payloads)?,
        _ => return Err(DeliveryProblem::UnsupportedSchema.error()),
    };
    let record = match native {
        Decoded::Record(record) => *record,
        Decoded::Unavailable(reason) => return Ok(Decoded::Unavailable(reason)),
    };
    payloads::validate_inventory(&payloads, &record)?;
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

fn activity(values: &Map<String, Value>, references: &payloads::References<'_>) -> Result<Decoded<TelemetryRecord>> {
    let mut facts = values.get("ActivityFacts").and_then(Value::as_object).cloned().ok_or_else(invalid)?;
    let statement = match payloads::definition(references, "/record/data/statement")? {
        Decoded::Record(statement) => statement,
        Decoded::Unavailable(reason) => return Ok(Decoded::Unavailable(reason)),
    };
    let original = references.get("/record/data/statement").ok_or_else(invalid)?;
    if facts.get("statement") != Some(*original) {
        return Err(invalid());
    }
    facts.insert("statement".into(), serde_json::to_value(statement).map_err(|_| invalid())?);
    let row: ActivityObservation = serde_json::from_value(Value::Object(facts)).map_err(|_| invalid())?;
    Ok(Decoded::Record(Box::new(TelemetryRecord::from(row))))
}

fn text<'a>(values: &'a Map<String, Value>, key: &str) -> Result<&'a str> {
    values.get(key).and_then(Value::as_str).ok_or_else(invalid)
}
fn invalid() -> culprit_core::Error {
    DeliveryProblem::Integrity.error()
}
