//! Shared analytical preparation and proofs; destination crates own physical layouts and query dialects.
pub mod catalog;
pub mod encoding;
pub mod envelope;
pub mod evidence;
pub mod indexes;
mod investigations;
pub mod prepared;
pub mod projection;
pub mod publication;
pub mod read_store;
pub mod receipts;
pub mod schema;
pub mod semantics;
pub mod sink;

use culprit_core::{Result, telemetry::TelemetryStream};
use schema::OutputColumn;
use serde_json::Value;

pub trait RowProjection: Send + Sync {
    fn from_logical(schema: schema::LogicalSchema) -> Result<Self>
    where
        Self: Sized;
    fn table(&self) -> &str;
    fn digest(&self) -> &str;
    fn columns(&self) -> &[OutputColumn];
    fn project(&self, source: &Value) -> Result<Vec<Value>>;
    fn encode(&self, values: &[Value]) -> Result<Vec<u8>>;
    fn returned_digest(&self, values: &[Value]) -> Result<[u8; 32]>;
}
pub trait ProjectionCatalog: Send + Sync {
    fn digest(&self) -> &str;
    fn schema(&self, stream: TelemetryStream) -> Result<&dyn RowProjection>;
    fn streams(&self) -> Vec<(TelemetryStream, &dyn RowProjection)>;
    fn publication(&self) -> &dyn RowProjection;
}

#[cfg(any(test, feature = "test-support"))]
pub mod test_support;
