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
        let mut characters = text.chars();
        let text = characters.by_ref().take(SQL_PREVIEW_CHARS).collect();
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
