//! ADX composes one analytical lifecycle with Kusto readback and a selected ingestion transport.
use crate::{
    catalog::SchemaCatalog,
    client::QueryTransport,
    control::AdxPublication,
    ingestion::{IngestionBatch, IngestionTransport},
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
        contract::Contract,
        object_store::transport::ObjectTransport,
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
    ingestion: Arc<dyn IngestionTransport>,
    readback: AdxReadback,
    control: AdxPublication,
}
impl AdxStore {
    pub fn new(query: Arc<dyn QueryTransport>, ingestion: Arc<dyn IngestionTransport>) -> Result<Self> {
        let catalog = Arc::new(SchemaCatalog::new()?);
        Ok(Self {
            readback: AdxReadback::new(query.clone(), catalog.clone()),
            control: AdxPublication::new(catalog.clone(), query.clone(), ingestion.clone()),
            ingestion,
            catalog,
            query,
        })
    }
}
pub fn new(
    contract: Contract,
    recovery: Arc<dyn RecoveryStore>,
    validation: Arc<dyn ObjectTransport>,
    ingestion: Arc<dyn IngestionTransport>,
    query: Arc<dyn QueryTransport>,
    scratch: &Path,
) -> Result<AnalyticalSink> {
    let indexes = Arc::new(ProjectionIndexes::new(validation));
    AnalyticalSink::new(contract, recovery, indexes, Arc::new(AdxStore::new(query, ingestion)?), scratch)
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
        self.ingestion.submit(Arc::new(IngestionBatch::encoded(range, attempt, batch)?)).await?;
        Ok(Submission::Accepted)
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
