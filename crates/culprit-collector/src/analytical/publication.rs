//! Deterministic completion and full content verification, including successful-empty captures.
use super::{ProjectionCatalog, envelope::capture_context, projection, sink::AnalyticalStore};
use crate::delivery::{
    recovery::CommittedRange,
    sink::{DeliveryAttempt, Publication, VerifiedRange},
};
use culprit_core::{
    Result,
    telemetry::{DeliveryProblem, digest},
};
use serde_json::{Value, json};
use std::sync::Arc;
pub struct ControlPublisher {
    catalog: Arc<dyn ProjectionCatalog>,
    store: Arc<dyn AnalyticalStore>,
}
impl ControlPublisher {
    pub fn new(catalog: Arc<dyn ProjectionCatalog>, store: Arc<dyn AnalyticalStore>) -> Self {
        Self { catalog, store }
    }
    pub async fn publish(&self, verified: &VerifiedRange, attempt: &DeliveryAttempt) -> Result<Publication> {
        let range = verified.committed();
        self.validate(range)?;
        if verified.receipts_digest() != range.manifest().receipts_digest()? {
            return Err(invalid());
        }
        let input = self.input(range)?;
        let schema = self.catalog.publication();
        let values = schema.project(&input)?;
        let bytes = schema.encode(&values)?;
        let id = input["publication_id"].as_str().ok_or_else(invalid)?;
        self.store.publish(range, attempt, id, bytes).await?;
        Ok(Publication { range_id: range.manifest().range_id.clone(), manifest_digest: range.manifest_digest().into() })
    }
    pub async fn visible(&self, publication: &Publication, range: &CommittedRange) -> Result<bool> {
        self.validate(range)?;
        if publication.range_id != range.manifest().range_id || publication.manifest_digest != range.manifest_digest() {
            return Err(invalid());
        }
        let schema = self.catalog.publication();
        let wanted = projection::digest(&schema.project(&self.input(range)?)?)?;
        let rows = self.store.publication_rows(range).await?;
        if rows.len() > 1 {
            return Err(invalid());
        }
        let Some(row) = rows.first() else {
            return Ok(false);
        };
        if schema.returned_digest(row)? != wanted {
            return Err(invalid());
        }
        Ok(true)
    }
    fn validate(&self, range: &CommittedRange) -> Result<()> {
        let contract = &range.manifest().contract;
        if contract.destination() != self.store.destination() {
            return Err(DeliveryProblem::DestinationChanged.error());
        }
        if contract.schema_digest() != self.catalog.digest() {
            return Err(DeliveryProblem::UnsupportedSchema.error());
        }
        Ok(())
    }
    pub fn input(&self, range: &CommittedRange) -> Result<Value> {
        let manifest = range.manifest();
        let receipts = manifest.receipts_digest()?;
        let publication_id = digest(
            &serde_json::to_vec(&(range.store_id(), range.manifest_digest(), &receipts)).map_err(|_| invalid())?,
        );
        let batches=manifest.batches.iter().map(|batch|{
            let table=if batch.delivery_required {Some(self.catalog.schema(batch.stream)?.table())} else {None};
            Ok(json!({"batch_id":batch.batch_id,"stream":batch.stream,"table":table,"rows":batch.rows.len(),
                "native_object":batch.object,"native_sha256":batch.sha256,"bytes":batch.bytes,"delivery_required":batch.delivery_required}))
        }).collect::<Result<Vec<_>>>()?;
        Ok(json!({"_culprit":capture_context(manifest,range.manifest_digest())?,"recovery_store_id":range.store_id(),
            "receipts_sha256":receipts,"publication_id":publication_id,"capture":manifest.capture,
            "source":manifest.source,"read_metrics":manifest.read_metrics,"batches":batches}))
    }
}
fn invalid() -> culprit_core::Error {
    DeliveryProblem::Integrity.error()
}
