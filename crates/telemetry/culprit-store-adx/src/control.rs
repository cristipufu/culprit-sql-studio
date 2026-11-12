//! Kusto completion staging and typed readback; deterministic completion content is store-independent.
use crate::{
    catalog::SchemaCatalog,
    client::{QueryRequest, QueryTransport},
    ingestion::{IngestionBatch, IngestionTransport},
};
use culprit_collector::delivery::{recovery::CommittedRange, sink::DeliveryAttempt};
use culprit_core::{Result, telemetry::DeliveryProblem};
use serde_json::{Value, json};
use std::{collections::BTreeMap, sync::Arc};
pub struct AdxPublication {
    catalog: Arc<SchemaCatalog>,
    query: Arc<dyn QueryTransport>,
    ingestion: Arc<dyn IngestionTransport>,
}
impl AdxPublication {
    pub fn new(
        catalog: Arc<SchemaCatalog>,
        query: Arc<dyn QueryTransport>,
        ingestion: Arc<dyn IngestionTransport>,
    ) -> Self {
        Self { catalog, query, ingestion }
    }
    pub async fn publish(
        &self,
        range: &CommittedRange,
        attempt: &DeliveryAttempt,
        id: &str,
        bytes: Vec<u8>,
    ) -> Result<()> {
        let batch = IngestionBatch::publication(self.catalog.publication().table(), range, id, attempt, bytes)?;
        self.ingestion.submit(Arc::new(batch)).await
    }
    pub async fn rows(&self, range: &CommittedRange) -> Result<Vec<Vec<Value>>> {
        let schema = self.catalog.publication();
        let names = schema.columns().iter().map(|column| column.name.as_str()).collect::<Vec<_>>().join(", ");
        let text = format!(
            "declare query_parameters(pRange:string);\n{}\n| where RangeId == pRange\n| project {}\n| distinct {}\n| take 2",
            schema.table(),
            schema.verification_projection(),
            names
        );
        let columns = schema.verification_columns();
        let table = self
            .query
            .query(QueryRequest {
                text,
                parameters: BTreeMap::from([("pRange".into(), json!(range.manifest().range_id))]),
                columns: columns.clone(),
                row_limit: 2,
            })
            .await?;
        if table.columns != columns {
            return Err(invalid());
        }
        Ok(table.rows)
    }
}
fn invalid() -> culprit_core::Error {
    DeliveryProblem::Integrity.error()
}
#[cfg(test)]
mod tests;
