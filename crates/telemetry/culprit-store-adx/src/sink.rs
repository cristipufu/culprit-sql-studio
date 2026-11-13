//! ADX composes one analytical lifecycle with Kusto readback and a selected ingestion transport.
use crate::{
    client::QueryTransport,
    control::AdxPublication,
    evidence::AdxReadStore,
    ingestion::{IngestionBatch, IngestionTransport},
};
use async_trait::async_trait;
use culprit_collector::{
    analytical::{
        ProjectionCatalog,
        encoding::EncodedBatch,
        indexes::ProjectionIndexes,
        read_store::{AnalyticalReadPage, AnalyticalReadStore},
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
    read: AdxReadStore,
    ingestion: Arc<dyn IngestionTransport>,
    control: AdxPublication,
}
impl AdxStore {
    pub fn new(query: Arc<dyn QueryTransport>, ingestion: Arc<dyn IngestionTransport>) -> Result<Self> {
        let read = AdxReadStore::new(query.clone())?;
        Ok(Self { control: AdxPublication::new(read.schema_catalog(), ingestion.clone()), ingestion, read })
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
impl AnalyticalReadStore for AdxStore {
    fn destination(&self) -> &str {
        self.read.destination()
    }
    fn catalog(&self) -> Arc<dyn ProjectionCatalog> {
        self.read.catalog()
    }
    fn logical_values(
        &self,
        stream: culprit_core::telemetry::TelemetryStream,
        row: &[Value],
    ) -> Result<serde_json::Map<String, Value>> {
        self.read.logical_values(stream, row)
    }
    async fn audit_membership(&self, range: &CommittedRange, byte_limit: usize) -> Result<usize> {
        self.read.audit_membership(range, byte_limit).await
    }
    async fn rows(
        &self,
        range: &CommittedRange,
        batch: &str,
        after: usize,
        until: usize,
        byte_limit: usize,
    ) -> Result<AnalyticalReadPage> {
        self.read.rows(range, batch, after, until, byte_limit).await
    }
    async fn publication_rows(&self, range: &CommittedRange, byte_limit: usize) -> Result<AnalyticalReadPage> {
        self.read.publication_rows(range, byte_limit).await
    }
}
#[async_trait]
impl AnalyticalStore for AdxStore {
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
    async fn publish(&self, range: &CommittedRange, attempt: &DeliveryAttempt, id: &str, bytes: Vec<u8>) -> Result<()> {
        self.control.publish(range, attempt, id, bytes).await
    }
}
