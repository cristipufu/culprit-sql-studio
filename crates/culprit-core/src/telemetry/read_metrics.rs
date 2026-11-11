//! Client-observed collection work. Native CPU and I/O costs remain separate database measurements.
use super::*;
pub const MAX_READ_TEMPLATES: usize = 32;
pub const MAX_READ_TEMPLATE_TEXT_BYTES: usize = 8 * 1024;
pub const MAX_READ_TEMPLATE_PREVIEW_BYTES: usize = 64 * 1024;
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct ReadQueryMetrics {
    pub template_id: String,
    pub statement: DefinitionPayload,
    pub completed_tasks: u32,
    pub failures: u32,
    pub timeouts: u32,
    pub cancellations: u32,
    pub unconfirmed_cancellations: u32,
    pub rows_decoded: String,
    /// Decoded values charged by the reader, not bytes read from disk or the network.
    pub charged_value_bytes: String,
    /// Includes transport and decoding after connection acquisition; it does not measure database CPU.
    pub client_elapsed_ms: f64,
}
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct ReadWorkMetrics {
    pub active_tasks: u32,
    pub untracked_tasks: u32,
    pub templates: Vec<ReadQueryMetrics>,
}
impl ReadWorkMetrics {
    pub fn has_unconfirmed_work(&self) -> bool {
        self.active_tasks > 0 || self.templates.iter().any(|metric| metric.unconfirmed_cancellations > 0)
    }
    pub fn is_valid(&self) -> bool {
        let mut ids = std::collections::BTreeSet::new();
        let mut preview_bytes = 0usize;
        let count = |value: &str| {
            !value.is_empty()
                && value.len() <= 20
                && value.bytes().all(|byte| byte.is_ascii_digit())
                && value.parse::<u64>().is_ok()
        };
        self.templates.len() <= MAX_READ_TEMPLATES
            && self.templates.iter().all(|metric| {
                let text_bounded = match &metric.statement {
                    DefinitionPayload::Available { text, .. } => text.len() <= MAX_READ_TEMPLATE_TEXT_BYTES,
                    DefinitionPayload::Unavailable { .. } => true,
                };
                let preview_bounded = serde_json::to_vec(&metric.statement).is_ok_and(|bytes| {
                    preview_bytes = preview_bytes.saturating_add(bytes.len());
                    preview_bytes <= MAX_READ_TEMPLATE_PREVIEW_BYTES
                });
                valid_digest(&metric.template_id)
                    && ids.insert(&metric.template_id)
                    && text_bounded
                    && preview_bounded
                    && metric.statement.is_valid()
                    && metric.client_elapsed_ms.is_finite()
                    && metric.client_elapsed_ms >= 0.0
                    && metric.failures <= metric.completed_tasks
                    && metric.timeouts <= metric.failures
                    && metric.cancellations <= metric.failures
                    && metric.unconfirmed_cancellations <= metric.failures
                    && count(&metric.rows_decoded)
                    && count(&metric.charged_value_bytes)
            })
    }
}
