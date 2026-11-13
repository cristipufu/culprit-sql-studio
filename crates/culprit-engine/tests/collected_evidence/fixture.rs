//! The actual recovery, encoding, immutable-index, publication and typed-reader path over a local warehouse stand-in.
use async_trait::async_trait;
use culprit_collector::{
    analytical::{
        ProjectionCatalog, RowProjection,
        catalog::PhysicalCatalog,
        encoding::BatchEncoder,
        evidence::{EvidenceCapture, EvidenceFamily, EvidenceReadBudget, EvidenceReader},
        indexes::ProjectionIndexes,
        publication,
        read_store::{AnalyticalReadPage, AnalyticalReadStore},
        test_support::{Blobs, Fault, JsonProjection as Schema, Recovery},
    },
    delivery::recovery::{CommittedRange, reopen},
};
use culprit_core::{Result, telemetry::*};
use serde_json::{Map, Value};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

struct Store {
    catalog: Arc<PhysicalCatalog<Schema>>,
    rows: Mutex<BTreeMap<String, Vec<Vec<Value>>>>,
    publication: Mutex<Option<Vec<Value>>>,
}
#[async_trait]
impl AnalyticalReadStore for Store {
    fn destination(&self) -> &str {
        "fixture:offline-analysis"
    }
    fn catalog(&self) -> Arc<dyn ProjectionCatalog> {
        self.catalog.clone()
    }
    fn logical_values(&self, stream: TelemetryStream, row: &[Value]) -> Result<Map<String, Value>> {
        self.catalog.get(stream)?.logical_values(row)
    }
    async fn audit_membership(&self, _: &CommittedRange, _: usize) -> Result<usize> {
        Ok(3)
    }
    async fn rows(
        &self,
        _: &CommittedRange,
        batch: &str,
        after: usize,
        until: usize,
        _: usize,
    ) -> Result<AnalyticalReadPage> {
        let rows = self.rows.lock().unwrap()[batch].iter().skip(after).take(until - after).cloned().collect::<Vec<_>>();
        Ok(AnalyticalReadPage { response_bytes: serde_json::to_vec(&rows).unwrap().len(), rows })
    }
    async fn publication_rows(&self, _: &CommittedRange, _: usize) -> Result<AnalyticalReadPage> {
        let rows = self.publication.lock().unwrap().clone().into_iter().collect::<Vec<_>>();
        Ok(AnalyticalReadPage { response_bytes: serde_json::to_vec(&rows).unwrap().len(), rows })
    }
}

pub(super) struct Fixture {
    pub range: CommittedRange,
    store: Arc<Store>,
    reader: EvidenceReader,
    _scratch: tempfile::TempDir,
}
impl Fixture {
    pub async fn new(source: CollectionDescription, records: Vec<TelemetryRecord>) -> Self {
        let catalog = Arc::new(PhysicalCatalog::<Schema>::new().unwrap());
        let mut recovery = Recovery::capture("fixture:offline-analysis", catalog.digest(), source, records, 100_000);
        if let Some(history) = recovery.manifest.source.query_history() {
            recovery.manifest.collected_from = history.interval.until.clone();
            recovery.manifest.collected_until = history.interval.until.clone();
        }
        let recovery = Arc::new(recovery);
        let range = reopen(recovery.as_ref(), &recovery.manifest.range_id).await.unwrap();
        let indexes = Arc::new(ProjectionIndexes::new(Arc::new(Blobs::default())));
        let scratch = tempfile::tempdir().unwrap();
        let encoder = BatchEncoder::new(catalog.clone(), recovery.clone(), scratch.path()).unwrap();
        let mut rows = BTreeMap::new();
        for batch in &range.manifest().batches {
            let encoded = encoder.encode(&range, &batch.batch_id).await.unwrap();
            indexes.put(&encoded.index_key, &encoded.digests).await.unwrap();
            let schema = catalog.get(batch.stream).unwrap();
            rows.insert(
                batch.batch_id.clone(),
                std::fs::read_to_string(encoded.path())
                    .unwrap()
                    .lines()
                    .map(|line| physical_row(schema, line.as_bytes()))
                    .collect(),
            );
        }
        let schema = catalog.publication();
        let publication = schema.project(&publication::input(catalog.as_ref(), &range).unwrap()).unwrap();
        let publication = physical_row(schema, &schema.encode(&publication).unwrap());
        let store = Arc::new(Store { catalog, rows: Mutex::new(rows), publication: Mutex::new(Some(publication)) });
        let reader = EvidenceReader::new(store.clone(), indexes);
        *recovery.fault.lock().unwrap() = Fault::Stall;
        Self { range, store, reader, _scratch: scratch }
    }
    pub fn remove_family(&self, stream: TelemetryStream) {
        let batch = self.range.manifest().batches.iter().find(|batch| batch.stream == stream).unwrap();
        self.store.rows.lock().unwrap().get_mut(&batch.batch_id).unwrap().clear();
    }
    pub fn remove_first_runtime(&self) {
        self.remove_first_row(TelemetryStream::QueryRuntime);
    }
    pub fn remove_first_row(&self, stream: TelemetryStream) {
        let batch = self.range.manifest().batches.iter().find(|batch| batch.stream == stream).unwrap();
        self.store.rows.lock().unwrap().get_mut(&batch.batch_id).unwrap().remove(0);
    }
    pub fn unpublish(&self) {
        self.store.publication.lock().unwrap().take();
    }
    pub async fn read(&self) -> EvidenceCapture {
        self.read_families(&[
            EvidenceFamily::QueryDefinitions,
            EvidenceFamily::PlanDefinitions,
            EvidenceFamily::QueryRuntime,
            EvidenceFamily::QueryWaits,
        ])
        .await
    }
    pub async fn read_families(&self, families: &[EvidenceFamily]) -> EvidenceCapture {
        self.reader.read_capture(&self.range, families, EvidenceReadBudget::default()).await.unwrap()
    }
}

fn physical_row(schema: &Schema, bytes: &[u8]) -> Vec<Value> {
    let object: Map<String, Value> = serde_json::from_slice(bytes).unwrap();
    schema.columns().iter().map(|column| object[&column.name].clone()).collect()
}
