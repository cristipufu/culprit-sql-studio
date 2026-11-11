use culprit_collector::delivery::{
    contract::Contract,
    evidence::{BatchManifest, RangeManifest, RowReceipt},
};
use culprit_core::telemetry::*;

pub(super) fn fixture() -> (RangeManifest, ExportedRecord) {
    let source = CollectionDescription {
        database: culprit_core::DatabaseIdentity::server(
            culprit_core::Engine::Mssql,
            "db",
            "fixture.invalid",
            1433,
            None,
        ),
        source: "resources".into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::RetainedSamples(SampleHistoryDescription {
            from: "2020-01-01T06:00:00Z".into(),
            until: "2020-01-01T06:01:00Z".into(),
            observed_at: "2020-01-01T06:02:00Z".into(),
            available_from: Some("2020-01-01T05:00:00Z".into()),
            available_until: Some("2020-01-01T06:01:30Z".into()),
            expected_samples: 1,
            nominal_period_ms: 15000,
        }),
    };
    let capture = CaptureIdentity {
        observation_id: "native-observation".into(),
        revision: Some(CaptureRevision { outbox_id: "outbox".into(), owner_epoch: "1".into(), sequence: "2".into() }),
    };
    let record = TelemetryRecord::ResourceSample(ResourceMeasurement {
        at: "2020-01-01T06:00:15Z".into(),
        replica_id: "primary".into(),
        metrics: vec![ResourceMetricSample {
            key: "cpu".into(),
            unit: ResourceUnit::Percent,
            scope: culprit_core::MeasurementScope::Database,
            statistic: ResourceStatistic::Mean,
            period_ms: Some(15000),
            limit_basis: ResourceLimitBasis::DatabaseLimit,
            value: Some(12.5),
        }],
    });
    let row = ExportedRecord::new(&source, &capture, record).unwrap();
    let mut bytes = serde_json::to_vec(&row).unwrap();
    bytes.push(b'\n');
    let sha256 = digest(&bytes);
    let batch = BatchManifest {
        batch_id: row.batch_id.clone(),
        stream: row.record.stream(),
        object: format!("payloads/{sha256}.ndjson"),
        sha256,
        bytes: bytes.len() as u64,
        rows: vec![RowReceipt::from_export(&row).unwrap()],
        delivery_required: true,
    };
    let contract = Contract::new("pipeline".into(), "shard-0".into(), "adx:test".into(), digest(b"schema")).unwrap();
    let range_id = digest(&serde_json::to_vec(&(&contract, &capture)).unwrap());
    let manifest = RangeManifest {
        contract,
        range_id,
        capture,
        source,
        collected_from: "2020-01-01T12:00:00Z".into(),
        collected_until: "2020-01-01T12:00:01Z".into(),
        read_metrics: None,
        batches: vec![batch],
        advances: vec![],
    };
    (manifest, row)
}

use crate::adx::catalog::SchemaCatalog;
use async_trait::async_trait;
use culprit_collector::delivery::{blob_store::transport::*, recovery::*};
use culprit_core::Result;
use std::{
    collections::BTreeMap,
    path::Path,
    pin::Pin,
    sync::Mutex,
    task::{Context, Poll},
};
use tokio::io::{AsyncRead, ReadBuf};

#[derive(Clone, Copy)]
pub(super) enum Fault {
    None,
    Corrupt,
    Truncate,
    LateError,
    Stall,
}
struct Input {
    bytes: Vec<u8>,
    offset: usize,
    fault: Fault,
}
impl AsyncRead for Input {
    fn poll_read(mut self: Pin<&mut Self>, _: &mut Context<'_>, buffer: &mut ReadBuf<'_>) -> Poll<std::io::Result<()>> {
        if self.offset == self.bytes.len() {
            match self.fault {
                Fault::LateError => return Poll::Ready(Err(std::io::Error::other("owned late failure"))),
                Fault::Stall => return Poll::Pending,
                _ => (),
            }
        }
        let count = buffer.remaining().min(self.bytes.len() - self.offset);
        buffer.put_slice(&self.bytes[self.offset..self.offset + count]);
        self.offset += count;
        Poll::Ready(Ok(()))
    }
}
pub(super) struct Recovery {
    pub(super) manifest: RangeManifest,
    objects: BTreeMap<String, Vec<u8>>,
    pub(super) fault: Mutex<Fault>,
}
impl Recovery {
    pub(super) fn make_empty(&mut self) {
        self.objects.clear();
        for batch in &mut self.manifest.batches {
            batch.bytes = 0;
            batch.sha256 = digest(b"");
            batch.object = format!("payloads/{}.ndjson", batch.sha256);
            batch.rows.clear();
            self.objects.insert(batch.object.clone(), vec![]);
        }
        if let SourceObservation::RetainedSamples(history) = &mut self.manifest.source.observation {
            history.expected_samples = 0;
        }
    }
    pub(super) fn new(catalog: &SchemaCatalog) -> Self {
        let (manifest, row) = fixture();
        Self::capture(catalog, manifest.source, vec![row.record], 2)
    }
    pub(super) fn capture(
        catalog: &SchemaCatalog,
        source: CollectionDescription,
        records: Vec<TelemetryRecord>,
        sequence: u64,
    ) -> Self {
        let (mut manifest, _) = fixture();
        manifest.source = source;
        manifest.capture.observation_id = format!("native-observation-{sequence}");
        manifest.capture.revision.as_mut().unwrap().sequence = sequence.to_string();
        manifest.contract =
            Contract::new("pipeline".into(), "shard-0".into(), "adx:test".into(), catalog.digest().into()).unwrap();
        manifest.range_id = digest(&serde_json::to_vec(&(&manifest.contract, &manifest.capture)).unwrap());
        let mut objects = BTreeMap::new();
        manifest.batches = manifest
            .source
            .observation
            .collection()
            .streams()
            .iter()
            .map(|stream| {
                let mut bytes = Vec::new();
                let mut rows = Vec::new();
                for record in records.iter().filter(|record| record.stream() == *stream) {
                    let exported = ExportedRecord::new(&manifest.source, &manifest.capture, record.clone()).unwrap();
                    rows.push(RowReceipt::from_export(&exported).unwrap());
                    serde_json::to_writer(&mut bytes, &exported).unwrap();
                    bytes.push(b'\n');
                }
                let sha256 = digest(&bytes);
                let object = format!("payloads/{sha256}.ndjson");
                let batch = BatchManifest {
                    batch_id: batch_id(&manifest.capture.observation_id, *stream).unwrap(),
                    stream: *stream,
                    object: object.clone(),
                    sha256,
                    bytes: bytes.len() as u64,
                    rows,
                    delivery_required: *stream != TelemetryStream::ActivityCheckpoints,
                };
                objects.insert(object, bytes);
                batch
            })
            .collect();
        manifest.validate().unwrap();
        Self { manifest, objects, fault: Mutex::new(Fault::None) }
    }
}
#[async_trait]
impl RecoveryStore for Recovery {
    async fn pending_ranges(
        &self,
        _: Option<&str>,
        _: u32,
        _: culprit_collector::delivery::recovery::RecoveryOrder,
    ) -> Result<RecoveryPage> {
        unreachable!("not a recovery enumeration fixture")
    }
    async fn retire(&self, _: &Fence, _: &culprit_collector::delivery::sink::PublishedRange) -> Result<()> {
        unreachable!("not a retirement fixture")
    }
    async fn activity_heads(&self, _: Option<&str>, _: u32) -> Result<ActivityHeadPage> {
        unreachable!()
    }
    fn id(&self) -> &str {
        "fixture-recovery"
    }
    async fn checkpoint_head(&self) -> Result<Option<String>> {
        unreachable!()
    }
    async fn captured_range(&self, _: &CapturedRange) -> Result<Option<CaptureIdentity>> {
        unreachable!()
    }
    async fn stage(&self, _: &PayloadObject, _: &Path) -> Result<StagedBatch> {
        unreachable!()
    }
    async fn commit(&self, _: &Fence, _: Option<&str>, _: &RangeManifest, _: &[StagedBatch]) -> Result<CommitReceipt> {
        unreachable!()
    }
    async fn load(&self, id: &str) -> Result<RangeManifest> {
        assert_eq!(id, self.manifest.range_id);
        Ok(self.manifest.clone())
    }
    async fn recover(&self, _: Option<&str>, _: u32) -> Result<RecoveryPage> {
        unreachable!()
    }
    async fn open_object(&self, batch: &PayloadObject) -> Result<RecoveryReader> {
        let fault = *self.fault.lock().unwrap();
        let mut bytes = self.objects.get(&batch.object).unwrap().clone();
        match fault {
            Fault::Corrupt => {
                *bytes.last_mut().unwrap() = b' ';
            }
            Fault::Truncate => {
                bytes.truncate(bytes.len() - 4);
            }
            _ => (),
        }
        Ok(Box::pin(Input { bytes, offset: 0, fault }))
    }
}
#[derive(Default)]
pub(super) struct Blobs {
    pub(super) objects: Mutex<BTreeMap<String, Vec<u8>>>,
    pub(super) uploads: Mutex<Vec<String>>,
}
#[async_trait]
impl BlobTransport for Blobs {
    async fn get(&self, key: &str) -> Result<Option<BlobRead>> {
        Ok(self.objects.lock().unwrap().get(key).cloned().map(|bytes| BlobRead {
            etag: "fixture".into(),
            bytes: bytes.len() as u64,
            body: Box::pin(std::io::Cursor::new(bytes)),
        }))
    }
    async fn put(&self, key: &str, bytes: Vec<u8>, condition: WriteCondition<'_>) -> Result<BlobWrite> {
        assert!(matches!(condition, WriteCondition::Absent));
        let mut objects = self.objects.lock().unwrap();
        if objects.contains_key(key) {
            return Ok(BlobWrite::ConditionNotMet);
        }
        objects.insert(key.into(), bytes);
        Ok(BlobWrite::Written)
    }
    async fn put_file(&self, key: &str, path: &Path, bytes: u64) -> Result<BlobWrite> {
        self.uploads.lock().unwrap().push(key.into());
        let body = std::fs::read(path).unwrap();
        assert_eq!(body.len() as u64, bytes);
        self.put(key, body, WriteCondition::Absent).await
    }
}

use crate::adx::{
    client::{QueryRequest, QueryTransport},
    response::QueryTable,
};
use serde_json::{Value, json};
use std::collections::VecDeque;

pub(super) enum Reply {
    Audit(u64),
    Rows(Vec<Vec<Value>>),
    Failure,
}
pub(super) struct Queries {
    pub(super) replies: Mutex<VecDeque<Reply>>,
    pub(super) requests: Mutex<Vec<QueryRequest>>,
}
#[async_trait]
impl QueryTransport for Queries {
    fn destination(&self) -> &str {
        "adx:test"
    }
    async fn query(&self, request: QueryRequest) -> Result<QueryTable> {
        let reply = self.replies.lock().unwrap().pop_front().expect("unexpected query");
        let columns = request.columns.clone();
        self.requests.lock().unwrap().push(request);
        match reply {
            Reply::Audit(count) => Ok(QueryTable { columns, rows: vec![vec![json!(count)]] }),
            Reply::Rows(rows) => Ok(QueryTable { columns, rows }),
            Reply::Failure => Err(DeliveryProblem::Transport.error()),
        }
    }
}

pub(super) fn interval_queries(catalog: &SchemaCatalog) -> Recovery {
    let (mut manifest, _) = fixture();
    manifest.source.database.engine = culprit_core::Engine::Postgres;
    manifest.source.source = "native-history-fixture".into();
    manifest.source.observation = SourceObservation::QueryHistory(QueryHistoryDescription {
        interval: TelemetryInterval {
            native_id: "native-period".into(),
            from: "2020-01-01T06:00:00Z".into(),
            until: "2020-01-01T07:00:00Z".into(),
        },
        dimensions: Default::default(),
        native_properties: Default::default(),
    });
    let identity =
        NativeQueryIdentity { query_id: "opaque-query".into(), qualifiers: [("userid".into(), "17".into())].into() };
    let references = StatementReferences {
        query: Some(NativeDefinitionReference {
            native: identity.native(),
            definition_id: Some(identity.definition_id(&manifest.source).unwrap()),
        }),
        plan: None,
    };
    let runtime = TelemetryRecord::QueryRuntime(RuntimeMeasurement {
        key: NativeMeasurementKey { native: NativeIdentity::new("runtime-1"), references: references.clone() },
        executions: "9007199254740993".into(),
        metrics: vec![
            RuntimeMetric::average(RuntimeMetricKey::Duration, RuntimeMetricUnit::Milliseconds, 1.25),
            RuntimeMetric::average(
                RuntimeMetricKey::LogicalReads,
                RuntimeMetricUnit::Pages { bytes_per_page: 16384 },
                7.0,
            ),
        ],
        first_execution_at: None,
        last_execution_at: None,
    });
    let waits = TelemetryRecord::QueryWaits(WaitMeasurement {
        key: NativeMeasurementKey { native: NativeIdentity::new("wait-1"), references },
        category: "native-io".into(),
        unit: RuntimeMetricUnit::Microseconds,
        total: MeasurementValue::Real(1.25),
        average: None,
        minimum: None,
        maximum: None,
    });
    let query = TelemetryRecord::QueryDefinition(QueryDefinition {
        id: identity.definition_id(&manifest.source).unwrap(),
        identity,
        origin: StatementOrigin::Application,
        monitoring_template_id: None,
        context: Default::default(),
        context_available: false,
        source_text_bytes: None,
        sql: DefinitionPayload::Unavailable { reason: PayloadUnavailable::NotCollected },
    });
    Recovery::capture(catalog, manifest.source, vec![runtime, waits, query], 9)
}

pub(super) fn cumulative_queries(catalog: &SchemaCatalog) -> Recovery {
    let (mut manifest, _) = fixture();
    manifest.source.database.engine = culprit_core::Engine::Postgres;
    manifest.source.source = "statement-counters-fixture".into();
    manifest.source.observation = SourceObservation::Snapshot(StateSnapshotDescription {
        dataset: SnapshotDataset::Operational,
        observed_from: "2020-01-01T12:00:00Z".into(),
        observed_until: "2020-01-01T12:00:01Z".into(),
        records: 1,
        sources: vec![StateSourceCoverage::collected("native-query-counters".into(), 1)],
    });
    let sample = StatementCounterSample {
        scope: culprit_core::MeasurementScope::Database,
        distributions: vec![],
        identity: NativeQueryIdentity {
            query_id: "fingerprint".into(),
            qualifiers: [("userid".into(), "17".into()), ("dbid".into(), "42".into())].into(),
        },
        observed_at: "2020-01-01T12:00:00Z".into(),
        reset: culprit_core::ResetMarker::Unknown,
        totals: vec![
            StatementCounter {
                key: RuntimeMetricKey::Executions,
                unit: RuntimeMetricUnit::Count,
                total: MeasurementValue::Integer("9007199254740993".into()),
            },
            StatementCounter {
                key: RuntimeMetricKey::Duration,
                unit: RuntimeMetricUnit::Milliseconds,
                total: MeasurementValue::Real(1.25),
            },
        ],
    };
    let row = TelemetryRecord::Operational(OperationalObservation {
        source: "native-query-counters".into(),
        key: sample.key().unwrap(),
        fact: OperationalFact::StatementCounters(Box::new(sample)),
    });
    Recovery::capture(catalog, manifest.source, vec![row], 10)
}
