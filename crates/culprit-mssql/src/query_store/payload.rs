//! Raw native payloads are private and never serialized. Consumers explicitly choose display or redacted export.
use crate::{read::invalid_data, value::Rec};
use culprit_core::{Result, telemetry::PayloadUnavailable};
#[derive(Clone)]
pub(crate) enum Payload {
    Text(String),
    Unavailable(PayloadUnavailable),
}
impl Payload {
    pub(crate) fn read(row: &Rec, bytes: Option<usize>, limit: usize) -> Result<Self> {
        Self::read_column(row, "payload", bytes, limit)
    }
    pub(crate) fn read_column(row: &Rec, column: &str, bytes: Option<usize>, limit: usize) -> Result<Self> {
        if row.bool("encrypted") {
            return Ok(Self::Unavailable(PayloadUnavailable::Encrypted));
        }
        if row.bool("restricted") {
            return Ok(Self::Unavailable(PayloadUnavailable::Restricted));
        }
        if bytes.is_some_and(|bytes| bytes > limit) {
            return Ok(Self::Unavailable(PayloadUnavailable::Oversized));
        }
        let Some(raw) = row.str(column).filter(|value| !value.is_empty()) else {
            return Ok(Self::Unavailable(PayloadUnavailable::Missing));
        };
        if bytes != Some(raw.encode_utf16().count() * 2) {
            return Err(invalid_data());
        }
        Ok(Self::Text(raw))
    }
    pub(crate) fn text(&self) -> Option<&str> {
        match self {
            Self::Text(text) => Some(text),
            Self::Unavailable(_) => None,
        }
    }
}
