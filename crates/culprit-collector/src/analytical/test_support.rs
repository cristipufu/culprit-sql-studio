//! Shared synthetic native evidence for destination conformance tests; never enabled in shipped adapters.
use crate::delivery::{
    contract::Contract,
    evidence::{BatchManifest, RangeManifest, RowReceipt},
};
use culprit_core::telemetry::*;

pub fn fixture() -> (RangeManifest, ExportedRecord) {
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
    let contract =
        Contract::new("pipeline".into(), "shard-0".into(), "fixture:test".into(), digest(b"schema")).unwrap();
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

use crate::delivery::{blob_store::transport::*, recovery::*};
use async_trait::async_trait;
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
pub enum Fault {
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
pub struct Recovery {
    pub manifest: RangeManifest,
    objects: BTreeMap<String, Vec<u8>>,
    pub fault: Mutex<Fault>,
}
impl Recovery {
    pub fn make_empty(&mut self) {
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
    pub fn new(destination: &str, schema: &str) -> Self {
        let (manifest, row) = fixture();
        Self::capture(destination, schema, manifest.source, vec![row.record], 2)
    }
    pub fn capture(
        destination: &str,
        schema: &str,
        source: CollectionDescription,
        records: Vec<TelemetryRecord>,
        sequence: u64,
    ) -> Self {
        let (mut manifest, _) = fixture();
        manifest.source = source;
        manifest.capture.observation_id = format!("native-observation-{sequence}");
        manifest.capture.revision.as_mut().unwrap().sequence = sequence.to_string();
        manifest.contract =
            Contract::new("pipeline".into(), "shard-0".into(), destination.into(), schema.into()).unwrap();
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
        _: crate::delivery::recovery::RecoveryOrder,
    ) -> Result<RecoveryPage> {
        unreachable!("not a recovery enumeration fixture")
    }
    async fn retire(&self, _: &Fence, _: &crate::delivery::sink::PublishedRange) -> Result<()> {
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
pub struct Blobs {
    pub objects: Mutex<BTreeMap<String, Vec<u8>>>,
    pub uploads: Mutex<Vec<String>>,
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

pub fn interval_queries(destination: &str, schema: &str) -> Recovery {
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
    Recovery::capture(destination, schema, manifest.source, vec![runtime, waits, query], 9)
}

pub fn cumulative_queries(destination: &str, schema: &str) -> Recovery {
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
    Recovery::capture(destination, schema, manifest.source, vec![row], 10)
}

pub fn activity(destination: &str, schema: &str, sequence: u64, empty: bool) -> Recovery {
    let (mut manifest, _) = fixture();
    manifest.source.source = "activity".into();
    manifest.source.observation = SourceObservation::Activity(ActivitySnapshotDescription {
        epoch: "native-epoch".into(),
        at: "2020-01-01T12:00:00Z".into(),
        scope: culprit_core::MeasurementScope::Database,
        complete_scope: true,
        policy: ActivityPolicy::default(),
        native_rows: u32::from(!empty),
        qualifying: u32::from(!empty),
        uncertain: 0,
        observations: u32::from(!empty),
        blocking_edges: 0,
        details: ActivityDetailStatus::Collected,
    });
    let record = TelemetryRecord::Activity(ActivityObservation {
        blocking_status: culprit_core::telemetry::ActivityBlockingStatus::Known,
        key: ActivityKey {
            session: NativeActivityIdentity { id: "61".into(), generation: "login-1".into() },
            request: Some(NativeActivityIdentity { id: "0".into(), generation: "request-1".into() }),
            transaction: None,
        },
        idle: false,
        scope: ActivityScopeBasis::RequestDatabase,
        state: "running".into(),
        database: Some("db".into()),
        command: Some("SELECT".into()),
        last_request_age_ms: None,
        open_transactions: 0,
        elapsed_ms: Some("120000".into()),
        transaction_age_ms: None,
        session_counters: ActivityCounters::default(),
        request_counters: Some(ActivityCounters::default()),
        wait: None,
        query_hash: None,
        client: None,
        statement: DefinitionPayload::Unavailable { reason: PayloadUnavailable::NotCollected },
    });
    Recovery::capture(destination, schema, manifest.source, if empty { vec![] } else { vec![record] }, sequence)
}

pub fn waits(destination: &str, schema: &str, sequence: u64, empty: bool, complete: bool) -> Recovery {
    let (mut manifest, _) = fixture();
    manifest.source.source = "wait-counters".into();
    manifest.source.observation = SourceObservation::Snapshot(StateSnapshotDescription {
        dataset: SnapshotDataset::WaitCounters,
        observed_from: "2020-01-01T12:00:00Z".into(),
        observed_until: "2020-01-01T12:00:01Z".into(),
        records: u32::from(!empty),
        sources: vec![StateSourceCoverage::observed("native-waits".into(), u32::from(!empty), complete)],
    });
    let row = TelemetryRecord::WaitCounter(WaitCounterObservation {
        source: "native-waits".into(),
        wait_type: "LCK_M_X".into(),
        scope: culprit_core::MeasurementScope::Database,
        reset: culprit_core::ResetMarker::Unknown,
        waiting_tasks_count: "9007199254740993".into(),
        wait_time_ms: "18446744073709551615".into(),
        signal_wait_time_ms: "0".into(),
        max_wait_time_ms: "7".into(),
    });
    Recovery::capture(destination, schema, manifest.source, if empty { vec![] } else { vec![row] }, sequence)
}

/// Wait for native completion to release the scratch lock, including destructors on another worker thread.
pub async fn released_spool(
    catalog: std::sync::Arc<dyn super::ProjectionCatalog>,
    recovery: std::sync::Arc<dyn RecoveryStore>,
    scratch: &Path,
) -> super::encoding::BatchEncoder {
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            if let Ok(encoder) = super::encoding::BatchEncoder::new(catalog.clone(), recovery.clone(), scratch) {
                break encoder;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("native completion did not release its spool owner")
}
