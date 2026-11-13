//! Restore only the proven payload inventory; native model validation and the exact native receipt still follow.
use super::*;
use std::collections::{BTreeMap, BTreeSet};

pub(super) type References<'a> = BTreeMap<&'a str, &'a Value>;

pub(super) fn definition(references: &References<'_>, pointer: &str) -> Result<Decoded<DefinitionPayload>> {
    if references.len() != 1 {
        return Err(invalid());
    }
    restore(references.get(pointer).ok_or_else(invalid)?)
}

pub(super) fn details(values: &Map<String, Value>, references: &References<'_>) -> Result<Decoded<Value>> {
    const FACT_DATA: &str = "/record/data/fact/data";
    let mut details = values.get("Details").ok_or_else(invalid)?.clone();
    for (pointer, reference) in references {
        let suffix = pointer
            .strip_prefix(FACT_DATA)
            .filter(|suffix| suffix.is_empty() || suffix.starts_with('/'))
            .ok_or_else(invalid)?;
        let target = details.pointer_mut(suffix).ok_or_else(invalid)?;
        if *target != **reference {
            return Err(invalid());
        }
        let payload = match restore(reference)? {
            Decoded::Record(payload) => payload,
            Decoded::Unavailable(reason) => return Ok(Decoded::Unavailable(reason)),
        };
        *target = serde_json::to_value(payload).map_err(|_| invalid())?;
    }
    Ok(Decoded::Record(Box::new(details)))
}

pub(super) fn validate_inventory(references: &References<'_>, record: &TelemetryRecord) -> Result<()> {
    let expected = record.definition_payloads().map(|field| field.pointer).collect::<BTreeSet<_>>();
    if !expected.iter().copied().eq(references.keys().copied()) {
        return Err(invalid());
    }
    Ok(())
}

pub(super) fn references(values: &Map<String, Value>) -> Result<References<'_>> {
    let references = values.get("DefinitionPayloads").and_then(Value::as_array).ok_or_else(invalid)?;
    let mut pointers = BTreeMap::new();
    for reference in references {
        let pointer = reference.get("pointer").and_then(Value::as_str).ok_or_else(invalid)?;
        if pointers.insert(pointer, reference).is_some() {
            return Err(invalid());
        }
    }
    Ok(pointers)
}

fn restore(reference: &Value) -> Result<Decoded<DefinitionPayload>> {
    let mut payload = reference.as_object().cloned().ok_or_else(invalid)?;
    payload.remove("pointer");
    if payload.get("state").and_then(Value::as_str) == Some("available") {
        let truncated = payload.remove("preview_truncated").and_then(|value| value.as_bool()).ok_or_else(invalid)?;
        if truncated {
            return Ok(Decoded::Unavailable(EvidenceGapReason::DetailInRecovery));
        }
        let text = payload.remove("preview").ok_or_else(invalid)?;
        let bytes = payload.remove("bytes").and_then(|value| value.as_u64()).ok_or_else(invalid)?;
        if text.as_str().is_none_or(|text| text.len() as u64 != bytes) {
            return Err(invalid());
        }
        payload.insert("text".into(), text);
    }
    Ok(match serde_json::from_value::<DefinitionPayload>(Value::Object(payload)) {
        Ok(payload) if payload.is_valid() => Decoded::Record(Box::new(payload)),
        _ => Decoded::Unavailable(EvidenceGapReason::NativeRepresentationRequired),
    })
}
