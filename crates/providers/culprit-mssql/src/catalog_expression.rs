//! Catalog bodies are bounded in SQL, checked against their native byte count and redacted before export.
use crate::{
    read::{invalid_data, optional_integer, optional_text},
    value::Rec,
};
use culprit_core::{Engine, Result, telemetry::*};

pub(crate) fn require_available(expression: &CatalogExpression) -> Result<()> {
    match &expression.payload {
        DefinitionPayload::Available { .. } => Ok(()),
        DefinitionPayload::Unavailable { reason: PayloadUnavailable::Oversized } => {
            Err(CollectionStop::ByteLimit.error())
        }
        DefinitionPayload::Unavailable { reason: PayloadUnavailable::Restricted } => {
            Err(crate::catalog_visibility::unavailable())
        }
        _ => Err(invalid_data()),
    }
}

pub(crate) fn select(expression: &str, prefix: &str, limit: usize) -> String {
    format!(
        "DATALENGTH({expression}) AS {prefix}_bytes, CASE WHEN DATALENGTH({expression})<={limit} THEN {expression} END AS {prefix}_value"
    )
}
pub(crate) fn decode(row: &Rec, present: bool, prefix: &str, limit: usize) -> Result<Option<CatalogExpression>> {
    decode_using(row, present, prefix, limit, CatalogExpression::sql)
}
pub(crate) fn decode_using(
    row: &Rec,
    present: bool,
    prefix: &str,
    limit: usize,
    make: fn(&str, Engine, usize) -> CatalogExpression,
) -> Result<Option<CatalogExpression>> {
    let bytes = optional_integer(row, &format!("{prefix}_bytes"))?;
    let raw = optional_text(row, &format!("{prefix}_value"))?;
    if !present {
        if bytes.is_some() || raw.is_some() {
            return Err(invalid_data());
        }
        return Ok(None);
    }
    let unavailable =
        |reason| CatalogExpression { native_sha256: None, payload: DefinitionPayload::Unavailable { reason } };
    let value = match (bytes, raw) {
        (Some(bytes), None) if bytes > limit as i64 => unavailable(PayloadUnavailable::Oversized),
        (Some(bytes), Some(raw)) if bytes <= limit as i64 && raw.encode_utf16().count() as i64 * 2 == bytes => {
            if raw.len() > limit {
                unavailable(PayloadUnavailable::Oversized)
            } else {
                make(&raw, Engine::Mssql, limit)
            }
        }
        (None, None) => unavailable(PayloadUnavailable::Restricted),
        _ => return Err(invalid_data()),
    };
    Ok(Some(value))
}
