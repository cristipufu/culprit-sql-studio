//! In-memory native transport only. Production encoding, indexes, receipts and publication own every proof.
use super::*;
use crate::analytical::{
    RowProjection,
    encoding::EncodedBatch,
    read_store::{AnalyticalReadPage, AnalyticalReadStore},
    sink::AnalyticalStore,
};
use serde_json::{Map, Value};
use std::{collections::BTreeMap, sync::Mutex};

pub(super) struct Warehouse {
    catalog: Arc<PhysicalCatalog<JsonProjection>>,
    rows: Mutex<BTreeMap<String, Vec<Vec<Value>>>>,
    publication: Mutex<Option<Vec<Value>>>,
}
impl Warehouse {
    pub fn new(catalog: Arc<PhysicalCatalog<JsonProjection>>) -> Self {
        Self { catalog, rows: Mutex::new(BTreeMap::new()), publication: Mutex::new(None) }
    }
    pub fn omit(&self, batch: &str, index: usize) {
        drop(self.rows.lock().unwrap().get_mut(batch).unwrap().remove(index));
    }
    fn page(rows: Vec<Vec<Value>>, limit: usize) -> Result<AnalyticalReadPage> {
        let response_bytes = serde_json::to_vec(&rows).unwrap().len();
        if response_bytes > limit {
            return Err(DeliveryProblem::Capacity.error());
        }
        Ok(AnalyticalReadPage { rows, response_bytes })
    }
}
#[async_trait::async_trait]
impl AnalyticalReadStore for Warehouse {
    fn destination(&self) -> &str {
        "fixture:informational"
    }
    fn catalog(&self) -> Arc<dyn ProjectionCatalog> {
        self.catalog.clone()
    }
    fn logical_values(&self, stream: TelemetryStream, row: &[Value]) -> Result<Map<String, Value>> {
        self.catalog.get(stream)?.logical_values(row)
    }
    async fn audit_membership(&self, _: &CommittedRange, _: usize) -> Result<usize> {
        Ok(1)
    }
    async fn rows(
        &self,
        range: &CommittedRange,
        batch_id: &str,
        after: usize,
        until: usize,
        limit: usize,
    ) -> Result<AnalyticalReadPage> {
        let batch = range.manifest().batches.iter().find(|batch| batch.batch_id == batch_id).unwrap();
        let schema = self.catalog.get(batch.stream)?;
        let ordinal = schema.columns().iter().position(|column| column.name == "RowOrdinal").unwrap();
        let rows = self.rows.lock().unwrap();
        let selected = rows
            .get(batch_id)
            .into_iter()
            .flatten()
            .filter(|row| row[ordinal].as_u64().is_some_and(|n| n > after as u64 && n <= until as u64))
            .cloned()
            .collect();
        Self::page(selected, limit)
    }
    async fn publication_rows(&self, _: &CommittedRange, limit: usize) -> Result<AnalyticalReadPage> {
        Self::page(self.publication.lock().unwrap().clone().into_iter().collect(), limit)
    }
}
#[async_trait::async_trait]
impl AnalyticalStore for Warehouse {
    fn polling_mode(&self) -> &'static str {
        "informational-fixture"
    }
    async fn dispatch(
        &self,
        range: &CommittedRange,
        _: &DeliveryAttempt,
        encoded: Arc<EncodedBatch>,
    ) -> Result<Submission> {
        let batch = range.manifest().batches.iter().find(|batch| batch.batch_id == encoded.batch_id).unwrap();
        let schema = self.catalog.get(batch.stream)?;
        let rows = std::fs::read_to_string(encoded.path())
            .unwrap()
            .lines()
            .map(|line| {
                let fields: Map<String, Value> = serde_json::from_str(line).unwrap();
                schema.columns().iter().map(|column| fields[&column.name].clone()).collect()
            })
            .collect();
        self.rows.lock().unwrap().insert(batch.batch_id.clone(), rows);
        Ok(Submission::Accepted)
    }
    async fn publish(&self, _: &CommittedRange, _: &DeliveryAttempt, _: &str, bytes: Vec<u8>) -> Result<()> {
        let fields: Map<String, Value> = serde_json::from_slice(&bytes).unwrap();
        *self.publication.lock().unwrap() =
            Some(self.catalog.publication().columns().iter().map(|column| fields[&column.name].clone()).collect());
        Ok(())
    }
}
