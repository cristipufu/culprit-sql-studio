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
        || optional_text(values, "RecordId")? != expected.record_id.as_deref()
        || values.get("RowOrdinal").and_then(Value::as_u64) != Some(u64::from(expected.key.row_ordinal))
        || values.get("ReferencesComplete").and_then(Value::as_bool)
            != Some(range.manifest().reference_coverage.complete)
        || text(values, "NativePayloadDigest")? != expected.payload_digest
    {
        return Err(invalid());
    }
    let payloads = payloads::references(values)?;
    let native = match stream {
        TelemetryStream::ResourceSamples => Decoded::Record(Box::new(resources(values)?)),
        TelemetryStream::WaitCounterSamples => Decoded::Record(Box::new(wait_counter(values)?)),
        TelemetryStream::ActivityObservations => activity(values, &payloads)?,
        TelemetryStream::OperationalSamples
        | TelemetryStream::ObjectDefinitions
        | TelemetryStream::ReplicationSamples
        | TelemetryStream::TuningObservations => {
            let mut details = match fact_details(values, &payloads)? {
                Decoded::Record(details) => details,
                Decoded::Unavailable(reason) => return Ok(Decoded::Unavailable(reason)),
            };
            if stream == TelemetryStream::OperationalSamples && text(values, "FactKind")? == "diagnostic-revision" {
                let scope = details.pointer_mut("/episode/scope").ok_or_else(invalid)?;
                let reference = scope.as_object().ok_or_else(invalid)?;
                if reference.len() != 1 {
                    return Err(invalid());
                }
                let key = reference.get("key").and_then(Value::as_str).ok_or_else(invalid)?;
                let SourceObservation::Diagnostics(description) = &range.manifest().source.observation else {
                    return Err(invalid());
                };
                let original = description.scope_bindings.get(key).ok_or_else(invalid)?;
                if original.key().map_err(|_| invalid())? != key {
                    return Err(invalid());
                }
                *scope = serde_json::to_value(original).map_err(|_| invalid())?;
            }
            if stream == TelemetryStream::OperationalSamples && text(values, "FactKind")? == "page-wait" {
                crate::analytical::page_target_contexts::restore(&mut details, &range.manifest().source)?;
            }
            fact_record(stream, values, *details)?
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
    if RowReceipt::from_export(&row, expected.key.row_ordinal)? != *expected {
        // Projection proof was checked first. A native precision/representation unavailable in that projection
        // requires the private native payload, rather than inventing an exact row from its rounded values.
        return Ok(Decoded::Unavailable(EvidenceGapReason::NativeRepresentationRequired));
    }
    Ok(Decoded::Record(Box::new(row)))
}

fn fact_details(values: &Map<String, Value>, references: &payloads::References<'_>) -> Result<Decoded<Value>> {
    if text(values, "DetailState")? != "inline" {
        return Ok(Decoded::Unavailable(EvidenceGapReason::DetailInRecovery));
    }
    payloads::details(values, references)
}

fn fact_record(
    stream: TelemetryStream,
    values: &Map<String, Value>,
    details: Value,
) -> Result<Decoded<TelemetryRecord>> {
    let mut data = json!({
        "source": text(values, "NativeSource")?,
        "fact": { "kind": text(values, "FactKind")?, "data": details },
    });
    if !matches!(stream, TelemetryStream::TuningObservations | TelemetryStream::ReplicationSamples) {
        data["key"] = json!(text(values, "FactKey")?);
    }
    if stream == TelemetryStream::ObjectDefinitions {
        data["version"] = json!(text(values, "DefinitionVersion")?);
    }
    Ok(match serde_json::from_value(json!({"stream": stream, "data": data})) {
        Ok(record) => {
            let fact_key = match &record {
                TelemetryRecord::Tuning(observation) => Some(observation.fact.key()),
                TelemetryRecord::Replication(observation) => Some(observation.fact.key()),
                _ => None,
            };
            if let Some(key) = fact_key
                && key != text(values, "FactKey")?
            {
                return Err(invalid());
            }
            Decoded::Record(Box::new(record))
        }
        Err(_) => Decoded::Unavailable(EvidenceGapReason::NativeRepresentationRequired),
    })
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
    let data_io_accounting =
        serde_json::from_value(values.get("DataIoAccounting").ok_or_else(invalid)?.clone()).map_err(|_| invalid())?;
    Ok(TelemetryRecord::ResourceSample(ResourceMeasurement { data_io_accounting, at, replica_id, metrics }))
}

fn wait_counter(values: &Map<String, Value>) -> Result<TelemetryRecord> {
    let mechanism =
        serde_json::from_value(values.get("WaitMechanism").ok_or_else(invalid)?.clone()).map_err(|_| invalid())?;
    let scope =
        serde_json::from_value(values.get("MeasurementScope").ok_or_else(invalid)?.clone()).map_err(|_| invalid())?;
    let mut reset = json!({"kind":text(values,"ResetKind")?});
    if let Some(value) = optional_text(values, "ResetValue")? {
        reset["value"] = json!(value);
    }
    let reset = serde_json::from_value(reset).map_err(|_| invalid())?;
    Ok(TelemetryRecord::WaitCounter(WaitCounterObservation {
        mechanism,
        source: text(values, "NativeSource")?.into(),
        wait_type: text(values, "WaitType")?.into(),
        scope,
        reset,
        waiting_tasks_count: text(values, "WaitingTasks")?.into(),
        wait_time_ms: text(values, "WaitTimeMs")?.into(),
        signal_wait_time_ms: text(values, "SignalWaitTimeMs")?.into(),
        max_wait_time_ms: text(values, "MaxWaitTimeMs")?.into(),
    }))
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
fn optional_text<'a>(values: &'a Map<String, Value>, key: &str) -> Result<Option<&'a str>> {
    match values.get(key) {
        Some(Value::Null) => Ok(None),
        Some(Value::String(text)) => Ok(Some(text)),
        _ => Err(invalid()),
    }
}
fn invalid() -> culprit_core::Error {
    DeliveryProblem::Integrity.error()
}
