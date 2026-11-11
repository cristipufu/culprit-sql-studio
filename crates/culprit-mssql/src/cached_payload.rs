//! Shared length probes and native-body decoding. A missing body is explicit; text is never truncated.
use crate::{
    query_store::payload::Payload,
    read::{invalid_data, optional_integer},
    value::Rec,
};
use culprit_core::{Engine, Result, telemetry::*};
pub(crate) fn columns(expression: &str, bytes_alias: &str, value_alias: &str, limit: &str) -> String {
    format!(
        "CAST(DATALENGTH({expression}) AS bigint) AS {bytes_alias},CASE WHEN DATALENGTH({expression})<={limit} THEN {expression} END AS {value_alias}"
    )
}
pub(crate) fn decode(
    row: &Rec,
    bytes_name: &str,
    value_name: &str,
    maximum: usize,
    redact: fn(&str, Engine) -> DefinitionPayload,
) -> Result<MssqlCachedPayload> {
    let bytes =
        optional_integer(row, bytes_name)?.map(|value| value.try_into().map_err(|_| invalid_data())).transpose()?;
    let body = Payload::read_column(row, value_name, bytes, maximum)?;
    from_raw(body, bytes, maximum, redact)
}
pub(crate) fn from_raw(
    body: Payload,
    bytes: Option<usize>,
    maximum: usize,
    redact: fn(&str, Engine) -> DefinitionPayload,
) -> Result<MssqlCachedPayload> {
    let native_sha256 = body.text().map(|text| digest(text.as_bytes()));
    let payload = match body {
        Payload::Text(raw) => {
            let payload = redact(&raw, Engine::Mssql);
            if raw.len() > maximum || matches!(&payload,DefinitionPayload::Available {text,..} if text.len()>maximum) {
                DefinitionPayload::Unavailable { reason: PayloadUnavailable::Oversized }
            } else {
                payload
            }
        }
        Payload::Unavailable(reason) => DefinitionPayload::Unavailable { reason },
    };
    let value =
        MssqlCachedPayload { source_utf16_bytes: bytes.map(|value: usize| value.to_string()), native_sha256, payload };
    if !value.is_valid() {
        return Err(invalid_data());
    }
    Ok(value)
}
