//! Reconstruct native query evidence only from the already proven canonical projection.
use super::*;

pub(super) fn record(
    stream: TelemetryStream,
    values: &Map<String, Value>,
    payloads: &super::payloads::References<'_>,
) -> Result<Decoded<TelemetryRecord>> {
    let data = match stream {
        TelemetryStream::QueryRuntime => json!({
            "key": measurement_key(values)?,
            "executions": field(values, "Executions")?,
            "metrics": field(values, "Metrics")?,
            "first_execution_at": field(values, "FirstExecutionAt")?,
            "last_execution_at": field(values, "LastExecutionAt")?,
        }),
        TelemetryStream::QueryWaits => json!({
            "key": measurement_key(values)?,
            "category": field(values, "WaitCategory")?,
            "unit": {"kind": text(values, "TimeUnit")?},
            "total": field(values, "TotalWait")?,
            "average": field(values, "AverageWait")?,
            "minimum": field(values, "MinimumWait")?,
            "maximum": field(values, "MaximumWait")?,
        }),
        TelemetryStream::QueryDefinitions | TelemetryStream::PlanDefinitions => {
            let data = definition(stream, values, payloads)?;
            match data {
                Decoded::Record(data) => *data,
                Decoded::Unavailable(reason) => return Ok(Decoded::Unavailable(reason)),
            }
        }
        _ => return Err(DeliveryProblem::UnsupportedSchema.error()),
    };
    let value = json!({"stream": stream, "data": data});
    Ok(match serde_json::from_value(value) {
        Ok(record) => Decoded::Record(Box::new(record)),
        Err(_) => Decoded::Unavailable(EvidenceGapReason::NativeRepresentationRequired),
    })
}

fn measurement_key(values: &Map<String, Value>) -> Result<Value> {
    Ok(json!({
        "native": {"id": text(values, "NativeRowId")?, "qualifiers": field(values, "NativeDimensions")?},
        "references": field(values, "NativeReferences")?,
    }))
}

fn definition(
    stream: TelemetryStream,
    values: &Map<String, Value>,
    payloads: &super::payloads::References<'_>,
) -> Result<Decoded<Value>> {
    let (field_name, pointer) = match stream {
        TelemetryStream::QueryDefinitions => ("sql", "/record/data/sql"),
        TelemetryStream::PlanDefinitions => ("plan", "/record/data/plan"),
        _ => return Err(DeliveryProblem::UnsupportedSchema.error()),
    };
    let payload = match super::payloads::definition(payloads, pointer)? {
        Decoded::Record(payload) => *payload,
        Decoded::Unavailable(reason) => return Ok(Decoded::Unavailable(reason)),
    };
    let mut data = json!({
        "id": text(values, "DefinitionId")?,
        "identity": field(values, "NativeIdentity")?,
        "source_text_bytes": field(values, "NativeSourceTextBytes")?,
    });
    let fields = data.as_object_mut().ok_or_else(invalid)?;
    fields.insert(field_name.into(), serde_json::to_value(payload).map_err(|_| invalid())?);
    match stream {
        TelemetryStream::QueryDefinitions => {
            for (key, column) in [
                ("origin", "StatementOrigin"),
                ("monitoring_template_id", "MonitoringTemplateId"),
                ("context", "CompilationContext"),
                ("context_available", "CompilationContextAvailable"),
            ] {
                fields.insert(key.into(), field(values, column)?.clone());
            }
        }
        TelemetryStream::PlanDefinitions => {
            for (key, column) in [
                ("query_definition_id", "QueryDefinitionId"),
                ("engine_version", "EngineVersion"),
                ("native_properties", "NativeProperties"),
            ] {
                fields.insert(key.into(), field(values, column)?.clone());
            }
        }
        _ => return Err(DeliveryProblem::UnsupportedSchema.error()),
    }
    Ok(Decoded::Record(Box::new(data)))
}

fn field<'a>(values: &'a Map<String, Value>, name: &str) -> Result<&'a Value> {
    values.get(name).ok_or_else(invalid)
}
