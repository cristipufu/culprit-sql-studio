//! Shared warehouse delivery: dispatch never proves visibility, and retirement requires exact readback plus publication.
use super::{
    ProjectionCatalog,
    encoding::{BatchEncoder, EncodedBatch},
    indexes::ProjectionIndexes,
    publication::ControlPublisher,
    receipts::ReceiptReader,
};
use crate::delivery::{
    contract::Contract,
    recovery::{CommittedRange, RecoveryStore},
    sink::*,
};
use async_trait::async_trait;
use culprit_core::{Result, telemetry::DeliveryProblem};
use serde_json::Value;
use std::{path::Path, sync::Arc};

/// Native warehouse operations. Implementations own wire/query details; no lifecycle or receipt policy is delegated.
#[async_trait]
pub trait AnalyticalStore: Send + Sync {
    fn destination(&self) -> &str;
    fn catalog(&self) -> Arc<dyn ProjectionCatalog>;
    fn polling_mode(&self) -> &'static str;
    async fn dispatch(
        &self,
        range: &CommittedRange,
        attempt: &DeliveryAttempt,
        batch: Arc<EncodedBatch>,
    ) -> Result<Submission>;
    async fn audit_membership(&self, range: &CommittedRange) -> Result<()>;
    async fn rows(&self, range: &CommittedRange, batch_id: &str, after: usize, until: usize)
    -> Result<Vec<Vec<Value>>>;
    async fn publish(&self, range: &CommittedRange, attempt: &DeliveryAttempt, id: &str, bytes: Vec<u8>) -> Result<()>;
    async fn publication_rows(&self, range: &CommittedRange) -> Result<Vec<Vec<Value>>>;
}

pub struct AnalyticalSink {
    contract: Contract,
    recovery_id: String,
    store: Arc<dyn AnalyticalStore>,
    encoder: BatchEncoder,
    indexes: Arc<ProjectionIndexes>,
    receipts: ReceiptReader,
    control: ControlPublisher,
}
impl AnalyticalSink {
    pub fn new(
        contract: Contract,
        recovery: Arc<dyn RecoveryStore>,
        indexes: Arc<ProjectionIndexes>,
        store: Arc<dyn AnalyticalStore>,
        scratch: &Path,
    ) -> Result<Self> {
        contract.validate()?;
        let catalog = store.catalog();
        if contract.destination() != store.destination() {
            return Err(DeliveryProblem::DestinationChanged.error());
        }
        if contract.schema_digest() != catalog.digest() {
            return Err(DeliveryProblem::UnsupportedSchema.error());
        }
        let recovery_id = recovery.id().to_owned();
        Ok(Self {
            contract,
            recovery_id,
            encoder: BatchEncoder::new(catalog.clone(), recovery, scratch)?,
            receipts: ReceiptReader::new(store.clone(), catalog.clone(), indexes.clone()),
            control: ControlPublisher::new(catalog, store.clone()),
            store,
            indexes,
        })
    }
    fn validate_range(&self, range: &CommittedRange) -> Result<()> {
        if range.manifest().contract != self.contract || range.store_id() != self.recovery_id {
            return Err(DeliveryProblem::DestinationChanged.error());
        }
        Ok(())
    }
}
#[async_trait]
impl TelemetrySink for AnalyticalSink {
    async fn validate_contract(&self, contract: &Contract) -> Result<()> {
        if contract != &self.contract {
            return Err(DeliveryProblem::DestinationChanged.error());
        }
        Ok(())
    }
    async fn dispatch(&self, range: &CommittedRange, attempt: &DeliveryAttempt, batch_id: &str) -> Result<Submission> {
        self.validate_range(range)?;
        attempt.validate()?;
        let batch = Arc::new(self.encoder.encode(range, batch_id).await?);
        self.indexes.put(&batch.index_key, &batch.digests).await?;
        self.store.dispatch(range, attempt, batch).await
    }
    async fn receipts(&self, range: &CommittedRange, cursor: Option<&str>, page_rows: u32) -> Result<ReceiptPage> {
        self.validate_range(range)?;
        self.receipts.receipts(range, cursor, page_rows).await
    }
    async fn publish(&self, range: &VerifiedRange, attempt: &DeliveryAttempt) -> Result<Publication> {
        self.validate_range(range.committed())?;
        attempt.validate()?;
        self.control.publish(range, attempt).await
    }
    async fn publication_visible(&self, publication: &Publication, range: &CommittedRange) -> Result<bool> {
        self.validate_range(range)?;
        self.control.visible(publication, range).await
    }
    async fn poll_updates(&self, cursor: Option<&str>, limit: u32) -> Result<DeliveryUpdates> {
        if limit == 0 || limit > 128 {
            return Err(DeliveryProblem::Capacity.error());
        }
        let mode = self.store.polling_mode();
        Ok(DeliveryUpdates {
            updates: vec![],
            next_cursor: mode.into(),
            expired: cursor.is_some_and(|cursor| cursor != mode),
        })
    }
}
