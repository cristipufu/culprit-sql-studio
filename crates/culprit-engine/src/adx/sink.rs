//! Destination composition: encoding, storage dispatch, exact readback and completion keep separate ownership.
use super::{
    catalog::SchemaCatalog, client::QueryTransport, control::ControlPublisher, encoding::BatchEncoder,
    indexes::ProjectionIndexes, ready::ReadyDispatcher, receipts::ReceiptReader,
};
use async_trait::async_trait;
use culprit_collector::delivery::{
    blob_store::transport::BlobTransport,
    contract::Contract,
    recovery::{CommittedRange, RecoveryStore},
    sink::*,
};
use culprit_core::{Result, telemetry::DeliveryProblem};
use std::{path::Path, sync::Arc};

pub struct AdxTelemetrySink {
    contract: Contract,
    recovery_id: String,
    dispatcher: ReadyDispatcher,
    receipts: ReceiptReader,
    control: ControlPublisher,
}
impl AdxTelemetrySink {
    pub fn new(
        contract: Contract,
        recovery: Arc<dyn RecoveryStore>,
        ready: Arc<dyn BlobTransport>,
        query: Arc<dyn QueryTransport>,
        scratch: &Path,
    ) -> Result<Self> {
        contract.validate()?;
        let catalog = Arc::new(SchemaCatalog::new()?);
        if contract.destination() != query.destination() {
            return Err(DeliveryProblem::DestinationChanged.error());
        }
        if contract.schema_digest() != catalog.digest() {
            return Err(DeliveryProblem::UnsupportedSchema.error());
        }
        let recovery_id = recovery.id().into();
        let indexes = Arc::new(ProjectionIndexes::new(ready.clone()));
        let encoder = BatchEncoder::new(catalog.clone(), recovery, scratch)?;
        Ok(Self {
            contract,
            recovery_id,
            dispatcher: ReadyDispatcher::new(encoder, indexes.clone(), ready.clone()),
            receipts: ReceiptReader::new(query.clone(), catalog.clone(), indexes),
            control: ControlPublisher::new(catalog, query, ready),
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
impl TelemetrySink for AdxTelemetrySink {
    async fn validate_contract(&self, contract: &Contract) -> Result<()> {
        if contract != &self.contract {
            return Err(DeliveryProblem::DestinationChanged.error());
        }
        Ok(())
    }
    async fn dispatch(&self, range: &CommittedRange, attempt: &DeliveryAttempt, batch_id: &str) -> Result<Submission> {
        self.validate_range(range)?;
        self.dispatcher.dispatch(range, attempt, batch_id).await
    }
    async fn receipts(&self, range: &CommittedRange, cursor: Option<&str>, page_rows: u32) -> Result<ReceiptPage> {
        self.validate_range(range)?;
        self.receipts.receipts(range, cursor, page_rows).await
    }
    async fn publish(&self, range: &VerifiedRange, attempt: &DeliveryAttempt) -> Result<Publication> {
        self.validate_range(range.committed())?;
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
        // This route settles by authoritative readback, without a destructive notification queue.
        // Changing a persisted polling mode asks the collector to reconcile retained ranges.
        const MODE: &str = "adx-readback-v1";
        Ok(DeliveryUpdates {
            updates: vec![],
            next_cursor: MODE.into(),
            expired: cursor.is_some_and(|cursor| cursor != MODE),
        })
    }
}
