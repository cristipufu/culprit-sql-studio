//! Read-only Kusto access to explicitly selected, independently verified capture evidence.
use crate::{catalog::SchemaCatalog, client::QueryTransport, receipts::AdxReadback};
use async_trait::async_trait;
use culprit_collector::{
    analytical::{
        ProjectionCatalog,
        evidence::EvidenceReader,
        indexes::ProjectionIndexes,
        read_store::{AnalyticalReadPage, AnalyticalReadStore},
    },
    delivery::recovery::CommittedRange,
};
use culprit_core::{Result, telemetry::TelemetryStream};
use serde_json::{Map, Value};
use std::sync::Arc;

pub struct AdxReadStore {
    query: Arc<dyn QueryTransport>,
    catalog: Arc<SchemaCatalog>,
    readback: AdxReadback,
}
impl AdxReadStore {
    pub fn new(query: Arc<dyn QueryTransport>) -> Result<Self> {
        let catalog = Arc::new(SchemaCatalog::new()?);
        Ok(Self { readback: AdxReadback::new(query.clone(), catalog.clone()), query, catalog })
    }
    pub(crate) fn schema_catalog(&self) -> Arc<SchemaCatalog> {
        self.catalog.clone()
    }
}

pub fn new(query: Arc<dyn QueryTransport>, indexes: Arc<ProjectionIndexes>) -> Result<EvidenceReader> {
    Ok(EvidenceReader::new(Arc::new(AdxReadStore::new(query)?), indexes))
}

#[async_trait]
impl AnalyticalReadStore for AdxReadStore {
    fn destination(&self) -> &str {
        self.query.destination()
    }
    fn catalog(&self) -> Arc<dyn ProjectionCatalog> {
        self.catalog.clone()
    }
    fn logical_values(&self, stream: TelemetryStream, row: &[Value]) -> Result<Map<String, Value>> {
        self.catalog.get(stream)?.logical_values(row)
    }
    async fn audit_membership(&self, range: &CommittedRange, byte_limit: usize) -> Result<usize> {
        self.readback.audit_membership(range, byte_limit).await
    }
    async fn rows(
        &self,
        range: &CommittedRange,
        batch: &str,
        after: usize,
        until: usize,
        byte_limit: usize,
    ) -> Result<AnalyticalReadPage> {
        self.readback.rows(range, batch, after, until, byte_limit).await
    }
    async fn publication_rows(&self, range: &CommittedRange, byte_limit: usize) -> Result<AnalyticalReadPage> {
        crate::control::read_rows(self.catalog.as_ref(), self.query.as_ref(), range, byte_limit).await
    }
}

#[cfg(test)]
mod tests;
