//! ADX composes the shared analytical lifecycle with Kusto queries and ready-blob dispatch.
use crate::{
    catalog::SchemaCatalog, client::QueryTransport, control::AdxPublication, ready::ReadyDispatcher,
    receipts::AdxReadback,
};
use async_trait::async_trait;
use culprit_collector::{
    analytical::{
        ProjectionCatalog,
        encoding::EncodedBatch,
        indexes::ProjectionIndexes,
        sink::{AnalyticalSink, AnalyticalStore},
    },
    delivery::{
        blob_store::transport::BlobTransport,
        contract::Contract,
        recovery::{CommittedRange, RecoveryStore},
        sink::{DeliveryAttempt, Submission},
    },
};
use culprit_core::Result;
use serde_json::Value;
use std::{path::Path, sync::Arc};

pub struct AdxStore {
    catalog: Arc<SchemaCatalog>,
    query: Arc<dyn QueryTransport>,
    dispatcher: ReadyDispatcher,
    readback: AdxReadback,
    control: AdxPublication,
}
impl AdxStore {
    pub fn new(query: Arc<dyn QueryTransport>, ready: Arc<dyn BlobTransport>) -> Result<Self> {
        let catalog = Arc::new(SchemaCatalog::new()?);
        Ok(Self {
            readback: AdxReadback::new(query.clone(), catalog.clone()),
            control: AdxPublication::new(catalog.clone(), query.clone(), ready.clone()),
            dispatcher: ReadyDispatcher::new(ready),
            catalog,
            query,
        })
    }
}
pub fn new(
    contract: Contract,
    recovery: Arc<dyn RecoveryStore>,
    ready: Arc<dyn BlobTransport>,
    query: Arc<dyn QueryTransport>,
    scratch: &Path,
) -> Result<AnalyticalSink> {
    let indexes = Arc::new(ProjectionIndexes::new(ready.clone()));
    AnalyticalSink::new(contract, recovery, indexes, Arc::new(AdxStore::new(query, ready)?), scratch)
}
#[async_trait]
impl AnalyticalStore for AdxStore {
    fn destination(&self) -> &str {
        self.query.destination()
    }
    fn catalog(&self) -> Arc<dyn ProjectionCatalog> {
        self.catalog.clone()
    }
    fn polling_mode(&self) -> &'static str {
        "adx-readback-v2"
    }
    async fn dispatch(
        &self,
        range: &CommittedRange,
        attempt: &DeliveryAttempt,
        batch: Arc<EncodedBatch>,
    ) -> Result<Submission> {
        self.dispatcher.dispatch(range, attempt, batch).await
    }
    async fn audit_membership(&self, range: &CommittedRange) -> Result<()> {
        self.readback.audit_membership(range).await
    }
    async fn rows(&self, range: &CommittedRange, batch: &str, after: usize, until: usize) -> Result<Vec<Vec<Value>>> {
        self.readback.rows(range, batch, after, until).await
    }
    async fn publish(&self, range: &CommittedRange, attempt: &DeliveryAttempt, id: &str, bytes: Vec<u8>) -> Result<()> {
        self.control.publish(range, attempt, id, bytes).await
    }
    async fn publication_rows(&self, range: &CommittedRange) -> Result<Vec<Vec<Value>>> {
        self.control.rows(range).await
    }
}
