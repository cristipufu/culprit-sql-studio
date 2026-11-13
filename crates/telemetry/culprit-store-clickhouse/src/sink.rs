//! The writer composes the same readback adapter with an explicit ingestion capability.
use crate::{
    client::{InsertData, QueryTransport},
    evidence::ClickHouseReadStore,
    ingestion::IngestionTransport,
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
use culprit_core::{Result, telemetry::DeliveryProblem};
use serde_json::Value;
use std::{path::Path, sync::Arc};

pub struct ClickHouseStore {
    read: ClickHouseReadStore,
    ingestion: Arc<dyn IngestionTransport>,
}
impl ClickHouseStore {
    pub fn new(query: Arc<dyn QueryTransport>, ingestion: Arc<dyn IngestionTransport>) -> Result<Self> {
        if query.destination() != ingestion.destination() {
            return Err(DeliveryProblem::DestinationChanged.error());
        }
        Ok(Self { read: ClickHouseReadStore::new(query)?, ingestion })
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
    AnalyticalSink::new(
        contract,
        recovery,
        Arc::new(ProjectionIndexes::new(validation)),
        Arc::new(ClickHouseStore::new(query, ingestion)?),
        scratch,
    )
}
#[async_trait]
impl AnalyticalReadStore for ClickHouseStore {
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
impl AnalyticalStore for ClickHouseStore {
    fn polling_mode(&self) -> &'static str {
        "clickhouse-readback-v1"
    }
    async fn dispatch(&self, _: &CommittedRange, _: &DeliveryAttempt, batch: Arc<EncodedBatch>) -> Result<Submission> {
        let table = batch.table.clone();
        self.ingestion.insert(&table, InsertData::Batch(batch)).await
    }
    async fn publish(&self, _: &CommittedRange, _: &DeliveryAttempt, _: &str, bytes: Vec<u8>) -> Result<()> {
        let catalog = self.read.catalog();
        match self.ingestion.insert(catalog.publication().table(), InsertData::Control(bytes)).await? {
            Submission::Accepted | Submission::Unknown => Ok(()),
            Submission::Rejected => Err(DeliveryProblem::Transport.error()),
        }
    }
}
