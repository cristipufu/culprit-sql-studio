use super::*;
use async_trait::async_trait;
use culprit_auth::CredentialResolver;
use culprit_collector::{
    analytical::{
        ProjectionCatalog, RowProjection,
        catalog::PhysicalCatalog,
        encoding::BatchEncoder,
        publication,
        read_store::{AnalyticalReadPage, AnalyticalReadStore},
        test_support::{JsonProjection, Recovery},
    },
    delivery::{
        object_store::{
            provider::{RecoveryProvider, RecoveryTarget},
            transport::*,
        },
        recovery::{CommittedRange, PayloadObject, RecoveryStore},
    },
};
use serde_json::{Map, Value};
use std::{
    collections::BTreeMap,
    path::Path,
    sync::{
        Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};
use tokio::io::AsyncReadExt;

#[derive(Default)]
pub(super) struct Objects {
    entries: Mutex<BTreeMap<String, (Vec<u8>, String)>>,
    revision: AtomicUsize,
    readonly: AtomicBool,
    pub(super) reads: Mutex<Vec<String>>,
    pub(super) stall: AtomicBool,
    pub(super) active: AtomicUsize,
}
impl Objects {
    pub(super) fn snapshot(&self) -> BTreeMap<String, (Vec<u8>, String)> {
        self.entries.lock().unwrap().clone()
    }
    pub(super) fn remove_proofs(&self) {
        self.entries.lock().unwrap().retain(|key, _| !key.starts_with("validation/"));
    }
    pub(super) fn remove_history(&self) {
        self.entries.lock().unwrap().clear();
    }
    pub(super) fn corrupt_manifest(&self) {
        let mut entries = self.entries.lock().unwrap();
        let (_, (bytes, _)) = entries.iter_mut().find(|(key, _)| key.starts_with("manifests/")).unwrap();
        bytes.push(b' ');
    }
}
struct PendingRead<'a>(&'a AtomicUsize);
impl Drop for PendingRead<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}
#[async_trait]
impl ObjectTransport for Objects {
    async fn get(&self, key: &str) -> Result<Option<ObjectRead>> {
        self.reads.lock().unwrap().push(key.into());
        if self.readonly.load(Ordering::SeqCst) {
            assert!(!key.starts_with("payloads/"), "investigation must not reread raw native payloads");
        }
        if self.stall.load(Ordering::SeqCst) {
            self.active.fetch_add(1, Ordering::SeqCst);
            let _pending = PendingRead(&self.active);
            std::future::pending::<()>().await;
        }
        Ok(self.entries.lock().unwrap().get(key).map(|(bytes, etag)| ObjectRead {
            etag: etag.clone(),
            bytes: bytes.len() as u64,
            body: Box::pin(std::io::Cursor::new(bytes.clone())),
        }))
    }
    async fn put(&self, key: &str, bytes: Vec<u8>, condition: WriteCondition<'_>) -> Result<ObjectWrite> {
        assert!(!self.readonly.load(Ordering::SeqCst), "archive investigation attempted to mutate recovery history");
        let mut entries = self.entries.lock().unwrap();
        let matches = match condition {
            WriteCondition::Absent => !entries.contains_key(key),
            WriteCondition::Matches(expected) => entries.get(key).is_some_and(|(_, etag)| etag == expected),
        };
        if !matches {
            return Ok(ObjectWrite::ConditionNotMet);
        }
        let etag = self.revision.fetch_add(1, Ordering::SeqCst).to_string();
        entries.insert(key.into(), (bytes, etag));
        Ok(ObjectWrite::Written)
    }
    async fn put_file(&self, key: &str, path: &Path, bytes: u64) -> Result<ObjectWrite> {
        let content = std::fs::read(path).unwrap();
        assert_eq!(content.len() as u64, bytes);
        self.put(key, content, WriteCondition::Absent).await
    }
}

pub(super) struct Store {
    catalog: Arc<PhysicalCatalog<JsonProjection>>,
    rows: Mutex<BTreeMap<String, Vec<Vec<Value>>>>,
    pub(super) publication: Mutex<Option<Vec<Value>>>,
    pub(super) requests: AtomicUsize,
    pub(super) denied: AtomicBool,
}
#[async_trait]
impl AnalyticalReadStore for Store {
    fn destination(&self) -> &str {
        "fixture:archive"
    }
    fn catalog(&self) -> Arc<dyn ProjectionCatalog> {
        self.catalog.clone()
    }
    fn logical_values(&self, stream: TelemetryStream, row: &[Value]) -> Result<Map<String, Value>> {
        self.catalog.get(stream)?.logical_values(row)
    }
    async fn audit_membership(&self, _: &CommittedRange, _: usize) -> Result<usize> {
        self.requests.fetch_add(1, Ordering::SeqCst);
        Ok(3)
    }
    async fn rows(
        &self,
        range: &CommittedRange,
        batch: &str,
        after: usize,
        until: usize,
        _: usize,
    ) -> Result<AnalyticalReadPage> {
        self.requests.fetch_add(1, Ordering::SeqCst);
        let stream = range.manifest().batches.iter().find(|candidate| candidate.batch_id == batch).unwrap().stream;
        let column = self.catalog.get(stream)?.columns().iter().position(|column| column.name == "RowOrdinal").unwrap();
        let rows = self.rows.lock().unwrap()[batch]
            .iter()
            .filter(|row| row[column].as_u64().is_some_and(|ordinal| ordinal > after as u64 && ordinal <= until as u64))
            .cloned()
            .collect::<Vec<_>>();
        Ok(AnalyticalReadPage { response_bytes: serde_json::to_vec(&rows).unwrap().len(), rows })
    }
    async fn publication_rows(&self, _: &CommittedRange, _: usize) -> Result<AnalyticalReadPage> {
        self.requests.fetch_add(1, Ordering::SeqCst);
        if self.denied.load(Ordering::SeqCst) {
            return Err(culprit_core::Error::new(culprit_core::ErrorCode::PermissionDenied, "owned fixture denied"));
        }
        let rows = self.publication.lock().unwrap().clone().into_iter().collect::<Vec<_>>();
        Ok(AnalyticalReadPage { response_bytes: serde_json::to_vec(&rows).unwrap().len(), rows })
    }
}
impl Store {
    pub(super) fn omit_first_row(&self) {
        self.rows.lock().unwrap().values_mut().next().unwrap().remove(0);
    }
    pub(super) fn corrupt_row(&self) {
        let schema = self.catalog.get(TelemetryStream::OperationalSamples).unwrap();
        let column = schema.columns().iter().position(|column| column.name == "FactKind").unwrap();
        self.rows.lock().unwrap().values_mut().next().unwrap()[0][column] = serde_json::json!("unproven-fact");
    }
}
struct Archive(Arc<Objects>);
impl RecoveryProvider for Archive {
    fn name(&self) -> &str {
        "fixture-recovery"
    }
    fn configure(&self, _: &Value, profile: &str, credentials: &dyn CredentialResolver) -> Result<RecoveryTarget> {
        credentials.bearer(profile)?;
        Ok(RecoveryTarget { transport: self.0.clone(), access_scope: "private-fixture".into() })
    }
}
struct Warehouse(Arc<Store>);
impl EvidenceStoreProvider for Warehouse {
    fn name(&self) -> &str {
        "fixture-warehouse"
    }
    fn configure(
        &self,
        _: &Value,
        profile: &str,
        credentials: &dyn CredentialResolver,
    ) -> Result<Arc<dyn AnalyticalReadStore>> {
        credentials.bearer(profile)?;
        Ok(self.0.clone())
    }
}

pub(super) struct Fixture {
    pub(super) objects: Arc<Objects>,
    pub(super) store: Arc<Store>,
    pub(super) service: EvidenceService,
    pub(super) request: EvidenceRequest,
    pub(super) config: EvidenceConfig,
    pub(super) runtime: CollectorRuntime,
    pub(super) profiles: Arc<super::super::profiles::Profiles>,
}
impl Fixture {
    pub(super) async fn new() -> Self {
        let (source, records) = culprit_collector::analytical::test_support::memory_failure_evidence();
        Self::capture(source, records).await
    }
    pub(super) async fn capture(source: CollectionDescription, records: Vec<TelemetryRecord>) -> Self {
        let objects = Arc::new(Objects::default());
        let catalog = Arc::new(PhysicalCatalog::<JsonProjection>::new().unwrap());
        let native = Recovery::capture("fixture:archive", catalog.digest(), source, records, 100_000);
        let manifest = native.manifest.clone();
        let archive = Arc::new(ObjectRecoveryStore::create(objects.clone(), manifest.contract.clone()).await.unwrap());
        let fence = archive.acquire().await.unwrap();
        let scratch = tempfile::tempdir().unwrap();
        let mut staged = Vec::new();
        for batch in &manifest.batches {
            let payload = PayloadObject::from(batch);
            let mut input = native.open_object(&payload).await.unwrap();
            let mut bytes = Vec::new();
            input.read_to_end(&mut bytes).await.unwrap();
            let path = scratch.path().join(&batch.batch_id);
            std::fs::write(&path, &bytes).unwrap();
            staged.push(archive.stage(&payload, &path).await.unwrap());
        }
        archive.commit(&fence, None, &manifest, &staged).await.unwrap();
        let range = reopen(archive.as_ref(), &manifest.range_id).await.unwrap();
        let indexes = ProjectionIndexes::new(objects.clone());
        let encoder = BatchEncoder::new(catalog.clone(), archive, scratch.path()).unwrap();
        let mut rows = BTreeMap::new();
        for batch in &manifest.batches {
            let encoded = encoder.encode(&range, &batch.batch_id).await.unwrap();
            indexes.put(&encoded.index_key, &encoded.digests).await.unwrap();
            rows.insert(
                batch.batch_id.clone(),
                std::fs::read_to_string(encoded.path())
                    .unwrap()
                    .lines()
                    .map(|line| physical_row(catalog.get(batch.stream).unwrap(), line.as_bytes()))
                    .collect(),
            );
        }
        let schema = catalog.publication();
        let values = schema.project(&publication::input(catalog.as_ref(), &range).unwrap()).unwrap();
        let publication = physical_row(schema, &schema.encode(&values).unwrap());
        let store = Arc::new(Store {
            catalog,
            rows: Mutex::new(rows),
            publication: Mutex::new(Some(publication)),
            requests: AtomicUsize::new(0),
            denied: AtomicBool::new(false),
        });
        let profiles = Arc::new(super::super::profiles::Profiles::default());
        let runtime = CollectorRuntime::empty()
            .with_credentials(profiles.clone())
            .with_recovery_provider(Arc::new(Archive(objects.clone())))
            .unwrap()
            .with_evidence_store_provider(Arc::new(Warehouse(store.clone())))
            .unwrap();
        let config = EvidenceConfig {
            pipeline: "pipeline".into(),
            shard: "shard-0".into(),
            credentials: BTreeMap::new(),
            recovery: crate::collector_service::config::RecoveryConfig {
                provider: "fixture-recovery".into(),
                options: serde_json::json!({}),
                credential: "archive-reader".into(),
            },
            warehouse: EvidenceStoreConfig {
                provider: "fixture-warehouse".into(),
                options: serde_json::json!({}),
                credential: "warehouse-reader".into(),
            },
        };
        let service = runtime.evidence(&config).unwrap();
        let request = EvidenceRequest {
            range_id: manifest.range_id,
            families: vec![EvidenceFamily::OperationalSamples],
            budget: EvidenceReadBudget { page_rows: 1, ..Default::default() },
        };
        objects.readonly.store(true, Ordering::SeqCst);
        objects.reads.lock().unwrap().clear();
        Self { objects, store, service, request, config, runtime, profiles }
    }
}
fn physical_row(schema: &JsonProjection, bytes: &[u8]) -> Vec<Value> {
    let values: Map<String, Value> = serde_json::from_slice(bytes).unwrap();
    schema.columns().iter().map(|column| values[&column.name].clone()).collect()
}
