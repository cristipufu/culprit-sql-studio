//! Compact presentation of a statement whose availability and redaction the original evidence owner established.
use culprit_core::telemetry::DefinitionPayload;
use serde_json::{Value, json};

const SQL_PREVIEW_CHARS: usize = 512;

struct SqlPreview {
    text: String,
    truncated: bool,
}
impl SqlPreview {
    fn new(text: &str) -> Self {
        Self::bounded(text, SQL_PREVIEW_CHARS)
    }
    fn bounded(text: &str, limit: usize) -> Self {
        let mut characters = text.chars();
        let text = characters.by_ref().take(limit).collect();
        Self { text, truncated: characters.next().is_some() }
    }
}

pub(crate) fn statement(payload: &DefinitionPayload) -> Value {
    match payload {
        DefinitionPayload::Available { text, .. } => {
            let preview = SqlPreview::new(text);
            json!({"availability":"available", "preview":preview.text, "preview_truncated":preview.truncated})
        }
        DefinitionPayload::Unavailable { reason } => json!({"availability":"unavailable", "reason":reason}),
    }
}

pub(crate) fn bounded_text(value: &str, limit: usize) -> String {
    let preview = SqlPreview::bounded(value, limit);
    let mut text = preview.text;
    if preview.truncated {
        text.push('…');
    }
    text
}
