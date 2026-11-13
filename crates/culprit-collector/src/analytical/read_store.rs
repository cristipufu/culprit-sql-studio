//! Read-only native warehouse operations, shared by delivery proofs and captured-evidence investigation.
use super::ProjectionCatalog;
use crate::delivery::recovery::CommittedRange;
use async_trait::async_trait;
use culprit_core::{Result, telemetry::TelemetryStream};
use serde_json::{Map, Value};
use std::sync::Arc;

pub const MAX_READ_RESPONSE_BYTES: usize = 16 * 1024 * 1024;

pub struct AnalyticalReadPage {
    pub rows: Vec<Vec<Value>>,
    /// Complete native response body, including metadata and completion frames.
    pub response_bytes: usize,
}

#[async_trait]
pub trait AnalyticalReadStore: Send + Sync {
    fn destination(&self) -> &str;
    fn catalog(&self) -> Arc<dyn ProjectionCatalog>;
    /// Restore logical nulls, timestamp units and JSON containers using the native physical schema.
    fn logical_values(&self, stream: TelemetryStream, row: &[Value]) -> Result<Map<String, Value>>;
    async fn audit_membership(&self, range: &CommittedRange, byte_limit: usize) -> Result<usize>;
    async fn rows(
        &self,
        range: &CommittedRange,
        batch_id: &str,
        after: usize,
        until: usize,
        byte_limit: usize,
    ) -> Result<AnalyticalReadPage>;
    async fn publication_rows(&self, range: &CommittedRange, byte_limit: usize) -> Result<AnalyticalReadPage>;
}
