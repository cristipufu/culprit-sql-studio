//! Target representation only: native recovery records remain unchanged and own every full payload.
use culprit_core::{Result, telemetry::*};
use serde_json::{Value, json};

pub const PREPARATION_VERSION: u32 = 2;
const MAX_PREVIEW_BYTES: usize = 1024;
const MAX_INLINE_DETAIL_BYTES: usize = 128 * 1024;

pub fn prepare(input: &mut Value, record: &TelemetryRecord, target: &CollectionTarget) -> Result<()> {
    input["_projection"] = json!({"payloads":[]});
    for field in record.definition_payloads() {
        let pointer = field.pointer;
        let payload = input.pointer_mut(pointer).ok_or_else(invalid)?;
        let mut reference = payload_reference(payload)?;
        reference["pointer"] = json!(pointer);
        *payload = reference.clone();
        input["_projection"]["payloads"].as_array_mut().ok_or_else(invalid)?.push(reference);
    }
    match record {
        TelemetryRecord::ResourceSample(row) => {
            let metrics = row
                .metrics
                .iter()
                .map(|metric| {
                    let mut value = serde_json::to_value(metric).map_err(|_| invalid())?;
                    value["describes_subject"] = json!(metric.describes_subject(target));
                    Ok(value)
                })
                .collect::<Result<Vec<_>>>()?;
            input["_projection"]["resource_metrics"] = json!(metrics);
        }
        TelemetryRecord::Activity(_) => input["_projection"]["activity"] = input["record"]["data"].clone(),
        TelemetryRecord::Incident(_) => {
            input["_projection"]["activity"] = input["record"]["data"]["state"]["observation"].clone();
        }
        TelemetryRecord::Operational(row) => fact(input, &row.key)?,
        TelemetryRecord::Replication(row) => fact(input, &row.fact.key())?,
        TelemetryRecord::Tuning(row) => fact(input, &row.fact.key())?,
        TelemetryRecord::ObjectDefinition(row) => fact(input, &row.key)?,
        TelemetryRecord::Deadlock(row) => deadlock(input, row)?,
        _ => (),
    }
    Ok(())
}

fn payload_reference(payload: &mut Value) -> Result<Value> {
    let reference = payload.as_object_mut().ok_or_else(invalid)?;
    if let Some(text) = reference.remove("text") {
        let text = text.as_str().ok_or_else(invalid)?;
        reference.insert("bytes".into(), json!(text.len()));
        reference.insert("preview".into(), json!(preview(text)));
        reference.insert("preview_truncated".into(), json!(text.len() > MAX_PREVIEW_BYTES));
    }
    Ok(Value::Object(std::mem::take(reference)))
}
fn preview(text: &str) -> &str {
    let mut end = text.len().min(MAX_PREVIEW_BYTES);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}
fn fact(input: &mut Value, key: &str) -> Result<()> {
    input["_projection"]["fact_key"] = json!(key);
    let details = input["record"]["data"]["fact"]["data"].clone();
    detail(input, details)
}
fn deadlock(input: &mut Value, row: &DeadlockEvent) -> Result<()> {
    input["_projection"]["process_count"] = json!(row.processes.len());
    input["_projection"]["resource_count"] = json!(row.resources.len());
    input["_projection"]["victim_count"] = json!(row.processes.iter().filter(|process| process.victim).count());
    let mut processes = serde_json::to_value(&row.processes).map_err(|_| invalid())?;
    for (process, native) in processes.as_array_mut().ok_or_else(invalid)?.iter_mut().zip(&row.processes) {
        process["statement"] = match &native.statement {
            Some(text) => json!({"sha256":digest(text.as_bytes()), "bytes":text.len(), "preview":preview(text),
                "preview_truncated":text.len()>MAX_PREVIEW_BYTES, "redaction_version":REDACTION_VERSION}),
            None => Value::Null,
        };
    }
    detail(input, json!({"processes":processes,"resources":row.resources}))
}
fn detail(input: &mut Value, details: Value) -> Result<()> {
    if !(details.is_object() || details.is_array()) {
        return Err(invalid());
    }
    let bytes = serde_json::to_vec(&details).map_err(|_| invalid())?;
    let inline = bytes.len() <= MAX_INLINE_DETAIL_BYTES;
    input["_projection"]["detail_state"] = json!(if inline { "inline" } else { "recovery-object" });
    input["_projection"]["detail_sha256"] = json!(digest(&bytes));
    input["_projection"]["details"] = if inline { details } else { Value::Null };
    Ok(())
}
fn invalid() -> culprit_core::Error {
    DeliveryProblem::Integrity.error()
}

#[cfg(test)]
mod tests;
