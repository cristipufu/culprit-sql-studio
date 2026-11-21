//! Shared synthetic native evidence for destination conformance tests; never enabled in shipped adapters.
use crate::delivery::{
    contract::Contract,
    evidence::{BatchManifest, RangeManifest, RowReceipt},
};
use culprit_core::telemetry::*;

mod suspect_pages;
pub use suspect_pages::{SuspectPageHistoryCase, assert_suspect_page_history_capture, suspect_page_history_evidence};
mod backup_history;
pub use backup_history::{BackupHistoryCase, assert_backup_history_capture, backup_history_evidence};
mod cdc;
mod change_capture;
pub use change_capture::multiple_errors as change_capture_multiple_errors;
mod change_feed;
pub use change_feed::{ChangeFeedCase, change_feed_evidence};
mod columnstore;
mod columnstore_rowgroups;
mod full_text_populations;
pub use cdc::{CdcCase, cdc_evidence};
pub use columnstore::columnstore_configuration_evidence_cases;
pub use columnstore_rowgroups::{ColumnstoreRowgroupCase, columnstore_rowgroup_evidence};
pub use full_text_populations::{FullTextPopulationCase, full_text_population_evidence};
mod table_memory;
pub use table_memory::{TableMemoryCase, table_memory_evidence};
mod table_temporal;
pub use table_temporal::{TableTemporalCase, table_temporal_evidence};
mod table_storage;
pub use table_storage::{TableStorageCase, table_storage_evidence};
mod table_cdc;
pub use table_cdc::{TableCdcCase, table_cdc_evidence};
mod table_full_text;
pub use table_full_text::{TableFullTextCase, assert_table_metadata_capture, table_full_text_evidence};
mod database_properties;
mod memory_clerks;
mod tempdb_metadata_memory;
pub use database_properties::{DatabasePropertiesCase, database_properties_evidence};
pub use memory_clerks::{MemoryClerkCase, memory_clerk_evidence};
pub use tempdb_metadata_memory::{
    TempdbMetadataMemoryCase, assert_tempdb_metadata_memory_capture, tempdb_metadata_memory_evidence,
};
mod counter_ratios;
mod performance_counters;
pub use performance_counters::{
    CommonPerformanceCounterCase, DatabasePerformanceCounterCase, PerformanceCounterCase, ResourceGovernorCounterCase,
    assert_common_performance_counter_capture, common_performance_counter_evidence,
    database_performance_counter_evidence, performance_counter_evidence, resource_governor_counter_evidence,
};
mod resource_io;
mod wait_counters;
pub use counter_ratios::{CounterRatioCase, counter_ratio_evidence};
pub use resource_io::{
    collected_resource_recovery, rbio_wait_evidence, resource_io_evidence, resource_role_evidence_cases,
};
pub use wait_counters::{WaitCounterCase, wait_counter_evidence};
mod activity;
mod session_census;
pub use session_census::{SessionCensusCase, session_census_evidence};
mod compilation_definitions;
mod compilation_intervals;
pub use activity::{
    activity_evidence_cases, activity_observation, assert_activity_capture, scalar_blocking_activity_evidence,
};
pub use compilation_definitions::{CompilationDefinitionCase, compilation_definition_evidence_cases};
mod blocking;
pub use blocking::{BlockingCase, assert_blocking_capture, blocking_evidence, blocking_evidence_cases};
pub use compilation_intervals::compilation_interval_evidence;
mod diagnostics;
pub use diagnostics::{assert_diagnostic_capture, diagnostic_evidence_cases};
mod source_overhead;
pub use source_overhead::{
    SOURCE_OVERHEAD_QUERY_COUNT, assert_native_source_overhead_summary, native_source_overhead_read_metrics,
    source_overhead_read_metrics,
};
mod source_partitions;
pub use source_partitions::{
    assert_replica_activity_capture, replica_activity_evidence_cases, replica_workload_evidence_cases,
    replicated_history_evidence_cases,
};
mod investigations;
pub use investigations::{
    InvestigationFixture, SharedInvestigationFixture, compiler_admission_evidence,
    compiler_admission_investigation_fixture, focused_request_memory_evidence,
    focused_request_memory_evidence_with_peers, investigation_fixture, request_memory_investigation_fixture,
    request_memory_investigation_fixture_with_peers, shared_investigation_evidence, shared_investigation_fixture,
};

mod queries;
pub use queries::{
    query_evidence, query_variant_evidence, query_wait_evidence_cases, query_wait_fragment_evidence, wait_semantics,
};
mod informational;
pub use informational::{assert_informational_capture, informational_query_evidence};
mod local_capture;
pub use local_capture::{LocalCaptureFixture, LocalSnapshotCase, local_snapshot_evidence_cases};
mod query_populations;
pub use query_populations::{
    assert_query_population_capture, query_population_evidence, query_population_evidence_cases,
};
mod page_server_reads;
mod query_cohorts;
pub use page_server_reads::{PageServerReadCase, assert_page_server_read_capture, page_server_read_evidence_cases};
pub use query_cohorts::{
    QueryCohortCase, assert_query_cohort_capture, query_cohort_evidence, query_cohort_evidence_cases,
};
mod tuning;
pub use tuning::{
    TuningCase, assert_tuning_capture, assert_tuning_scope_projection, change_tuning_plan_scope, tuning_evidence,
    tuning_evidence_cases, tuning_scope_evidence,
};
mod completed_spills;
pub use completed_spills::{CompletedSpillCase, completed_spill_evidence_cases};
mod cached_runtime;
pub use cached_runtime::cached_runtime_evidence_cases;
mod configured_log_rates;
mod hyperscale_log_components;
pub use hyperscale_log_components::hyperscale_log_component_evidence_cases;
mod file_io;
pub use configured_log_rates::configured_log_rate_evidence_cases;
pub use file_io::file_io_evidence_cases;
mod connection_outcomes;
pub use connection_outcomes::{assert_connection_outcome_capture, connection_outcome_evidence_cases};
mod cache_entry_memory;
pub use cache_entry_memory::{
    CacheEntryMemoryCase, assert_cache_entry_memory_capture, cache_entry_memory_comparison_evidence,
    cache_entry_memory_evidence, cache_entry_memory_evidence_cases,
};
mod cleanup;
mod observations;
pub use cleanup::{
    CleanupCase, assert_cleanup_capture, cleanup_evidence, cleanup_evidence_cases, cleanup_tamper_cases,
};
pub use observations::{
    memory_failure_evidence, memory_failure_observation, request_memory_evidence, retention_coverage_only_evidence,
    retention_evidence,
};
mod request_memory;
pub use request_memory::{
    maintenance_memory_evidence_cases, request_memory_evidence_cases, reservation_demand_evidence_cases,
    scalar_grant_pressure_evidence,
};
mod grant_output;
pub use grant_output::{
    GrantOutputCase, assert_grant_output_capture, grant_output_evidence, output_wait_counter_evidence,
    output_wait_grant_evidence,
};
mod session_memory;
pub use session_memory::session_memory_evidence_cases;
mod page_waits;
pub use page_waits::{page_wait_evidence_cases, page_wait_tamper_cases, shared_page_wait_evidence_cases};
mod page_task_holders;
mod page_writers;
pub use page_task_holders::{
    PageTaskHolderCase, PageTaskHolderFixture, page_task_holder_evidence, page_task_holder_evidence_cases,
    page_task_holder_fixture,
};
pub use page_writers::{
    PageWriterCase, PageWriterFixture, page_writer_evidence, page_writer_evidence_cases, page_writer_fixture,
};
mod current_request_work;
pub use current_request_work::{
    assert_current_request_work_capture, current_request_work_evidence_cases, postgres_copy_evidence_cases,
};
mod statement_work;
pub use statement_work::{
    node_statement_work_evidence_cases, statement_outcome_evidence_cases, statement_work_evidence_cases,
};
mod statement_grants;
pub use statement_grants::{
    StatementGrantCase, assert_statement_grant_capture, statement_grant_evidence, statement_grant_evidence_cases,
};
mod runtime_distributions;
pub use runtime_distributions::runtime_distribution_evidence_cases;
mod task_space;
pub use task_space::{assert_task_space_capture, task_space_evidence_cases};
mod object_definitions;
pub use object_definitions::{
    MemoryFeedbackCase, PageServerIoCase, ParameterEvidenceCase, TvpLoopCase, assert_object_definition_capture,
    memory_feedback_evidence, memory_feedback_evidence_cases, object_definition_bound_join_evidence,
    object_definition_evidence_cases, object_definition_hydration_evidence, object_definition_recovery_only_evidence,
    object_definition_retained_input_evidence, page_server_io_evidence, parameter_evidence, parameter_evidence_cases,
    tvp_loop_evidence, tvp_loop_evidence_cases,
};
mod projection;
pub use projection::JsonProjection;
mod native_failures;
pub use native_failures::native_failure_evidence_cases;
mod jobs;
mod mysql_events;
pub use mysql_events::{MysqlEventsCase, mysql_events_evidence, mysql_events_evidence_cases};
mod mssql_agent_jobs;
pub use jobs::{JobsCase, assert_jobs_capture, jobs_evidence, jobs_evidence_cases};
pub use mssql_agent_jobs::{MssqlAgentJobsCase, mssql_agent_jobs_evidence, mssql_agent_jobs_evidence_cases};
mod maintenance_progress;
pub use maintenance_progress::maintenance_progress_evidence_cases;
mod maintenance_blocking;
pub use maintenance_blocking::{
    MaintenanceBlockingCase, MaintenanceBlockingFixture, maintenance_activity_evidence, maintenance_blocking_evidence,
};
mod replication;
pub use replication::{ReplicationCase, assert_replication_capture, replication_evidence, replication_evidence_cases};

pub fn assert_memory_participants_capture(
    capture: &crate::analytical::evidence::EvidenceCapture,
    source: &CollectionDescription,
    records: &[TelemetryRecord],
) {
    assert_operational_capture(capture, source, records);
    let dataset = source.observation.snapshot().expect("native memory snapshot").dataset;
    match &source.target {
        CollectionTarget::Database(_) => assert_eq!(dataset, SnapshotDataset::CurrentRequestMemory),
        CollectionTarget::Shared(resource) => {
            assert_eq!(dataset, SnapshotDataset::SessionMemory);
            assert_eq!(resource.kind, SharedResourceKind::Instance);
        }
    }
}

pub fn assert_page_wait_capture(
    capture: &crate::analytical::evidence::EvidenceCapture,
    source: &CollectionDescription,
    records: &[TelemetryRecord],
) {
    assert_operational_capture(capture, source, records);
    assert_eq!(source.observation.snapshot().unwrap().dataset, SnapshotDataset::PageWaitTargets);
    for record in records {
        let TelemetryRecord::Operational(OperationalObservation { fact: OperationalFact::PageWait(page), .. }) = record
        else {
            panic!()
        };
        assert!(page.fits_source_scope(source));
        if let Some(target) = &page.target
            && let PageTargetScope::SharedResource { context } = &target.scope
        {
            assert_eq!(context.target, source.target);
            assert_eq!(context.partition, source.partition);
            assert_eq!(page.request.database.as_deref(), Some("db"));
            assert_eq!(target.header.database_id, 2);
        }
    }
}

pub fn assert_page_wait_scope_projection(details: &serde_json::Value, source: &CollectionDescription) {
    if matches!(source.target, CollectionTarget::Shared(_)) {
        let context = details.pointer("/target/scope/context").expect("shared context reference");
        assert_eq!(context["target"], serde_json::json!({"key":source.target.telemetry_id().unwrap()}));
        assert!(context["partition"]["key"].as_str().is_some_and(valid_digest));
        let public = serde_json::to_string(details).unwrap();
        assert!(!public.contains("private-tempdb-machine-canary"));
        assert!(!public.contains("private-tempdb-instance-canary"));
    }
}

pub fn assert_statement_work_capture(
    capture: &crate::analytical::evidence::EvidenceCapture,
    source: &CollectionDescription,
    records: &[TelemetryRecord],
) {
    assert_operational_capture(capture, source, records);
    assert!(source.target.database().is_some());
    assert_eq!(source.observation.snapshot().unwrap().dataset, SnapshotDataset::StatementCounters);
    let rows = capture
        .records()
        .iter()
        .map(|row| match &row.row().record {
            TelemetryRecord::Operational(observation) => observation.clone(),
            _ => panic!("native counter point"),
        })
        .collect();
    assert!(StatementCounterSnapshot::from_source(source, rows).unwrap().is_valid());
}

pub fn assert_runtime_distribution_capture(
    capture: &crate::analytical::evidence::EvidenceCapture,
    source: &CollectionDescription,
    records: &[TelemetryRecord],
) {
    assert_operational_capture(capture, source, records);
    assert!(source.target.database().is_some());
    assert_eq!(source.observation.snapshot().unwrap().dataset, SnapshotDataset::RuntimeDistributions);
}

pub fn assert_operational_capture(
    capture: &crate::analytical::evidence::EvidenceCapture,
    source: &CollectionDescription,
    records: &[TelemetryRecord],
) {
    assert!(capture.is_read_complete(), "{:?}", capture.gaps());
    assert_eq!(capture.source(), source);
    assert_eq!(capture.records().len(), records.len());
    let mut identities = std::collections::BTreeSet::new();
    for record in records {
        let expected = ExportedRecord::new(source, capture.capture(), record.clone()).unwrap();
        let actual = capture.records().iter().find(|row| row.row() == &expected).expect("exact native participant");
        assert!(identities.insert(actual.row().record_id.as_ref()));
        assert_redacted_definition_payloads(&actual.row().record);
    }
}

fn assert_redacted_definition_payloads(record: &TelemetryRecord) {
    for definition in record.definition_payloads() {
        if let DefinitionPayload::Available { text, .. } = definition.payload {
            assert!(!text.contains("9264"), "unredacted fixture literal in {}", definition.pointer);
        }
    }
}

pub fn fixture() -> (RangeManifest, ExportedRecord) {
    let source = CollectionDescription {
        partition: culprit_core::telemetry::SourcePartition::Target,
        target: culprit_core::DatabaseIdentity::server(
            culprit_core::Engine::Mssql,
            "db",
            "fixture.invalid",
            1433,
            None,
        )
        .into(),
        source: "resources".into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::RetainedSamples(SampleHistoryDescription {
            from: "2020-01-01T06:00:00Z".into(),
            until: "2020-01-01T06:01:00Z".into(),
            observed_at: "2020-01-01T06:02:00Z".into(),
            available_from: Some("2020-01-01T05:00:00Z".into()),
            available_until: Some("2020-01-01T06:01:30Z".into()),
            expected_samples: 1,
            nominal_period_ms: Some(15000),
        }),
    };
    let capture = CaptureIdentity {
        observation_id: "native-observation".into(),
        revision: Some(CaptureRevision { outbox_id: "outbox".into(), owner_epoch: "1".into(), sequence: "2".into() }),
    };
    let record = TelemetryRecord::ResourceSample(ResourceMeasurement {
        mssql_managed_instance: None,
        replica_role: None,
        data_io_accounting: culprit_core::telemetry::DataIoAccounting::Unknown,
        at: "2020-01-01T06:00:15Z".into(),
        replica_id: Some("primary".into()),
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
        rows: vec![RowReceipt::from_export(&row, 1).unwrap()],
        delivery_required: true,
    };
    let contract =
        Contract::new("pipeline".into(), "shard-0".into(), "fixture:test".into(), digest(b"schema")).unwrap();
    let range_id = digest(&serde_json::to_vec(&(&contract, &capture)).unwrap());
    let manifest = RangeManifest {
        capture_manifest_sha256: culprit_core::telemetry::digest(b"fixture-manifest"),
        investigation: None,
        contract,
        range_id,
        capture,
        reference_coverage: crate::collection_references::ObservationReferences::default()
            .coverage(true, Some(&source)),
        source,
        collected_from: "2020-01-01T12:00:00Z".into(),
        collected_until: "2020-01-01T12:00:01Z".into(),
        source_queries: 0,
        read_metrics: None,
        batches: vec![batch],
        advances: vec![],
    };
    (manifest, row)
}

/// A real export/recovery fixture for each shared scope; no database identity is invented for it.
pub fn shared_resource(kind: SharedResourceKind, namespace: &str) -> (CollectionDescription, TelemetryRecord) {
    let (mut manifest, mut row) = fixture();
    manifest.source.target = CollectionTarget::Shared(SharedResourceIdentity {
        engine: culprit_core::Engine::Postgres,
        kind,
        namespace: NativeIdentity::new(namespace),
        native: NativeIdentity::new("resource"),
    });
    let TelemetryRecord::ResourceSample(sample) = &mut row.record else { unreachable!() };
    sample.metrics[0].scope = manifest.source.target.measurement_scope();
    sample.metrics[0].limit_basis = ResourceLimitBasis::Absolute;
    (manifest.source, row.record)
}

/// Pool history supplies no replica or promised period; host context is broader than the measured pool.
pub fn pool_history() -> (CollectionDescription, TelemetryRecord) {
    let (mut source, mut record) = shared_resource(SharedResourceKind::Pool, "fixture-server");
    let CollectionTarget::Shared(resource) = &mut source.target else { unreachable!() };
    resource.engine = culprit_core::Engine::Mssql;
    source.source = culprit_core::source_id::POOL_RESOURCES.into();
    let SourceObservation::RetainedSamples(history) = &mut source.observation else { unreachable!() };
    history.nominal_period_ms = None;
    let TelemetryRecord::ResourceSample(sample) = &mut record else { unreachable!() };
    sample.replica_id = None;
    let cpu = &mut sample.metrics[0];
    cpu.period_ms = None;
    cpu.limit_basis = ResourceLimitBasis::PoolLimit;
    cpu.value = Some(0.0);
    let mut io = cpu.clone();
    io.key = "data_io".into();
    io.value = None;
    let mut storage = cpu.clone();
    storage.key = "used_storage".into();
    storage.statistic = ResourceStatistic::Point;
    storage.unit = ResourceUnit::Bytes;
    storage.limit_basis = ResourceLimitBasis::Absolute;
    storage.value = Some(512.0 * 1024.0 * 1024.0);
    let mut host = cpu.clone();
    host.key = "instance_memory".into();
    host.scope = culprit_core::MeasurementScope::Server;
    host.limit_basis = ResourceLimitBasis::InstanceLimit;
    host.value = Some(65.0);
    sample.metrics.extend([io, storage, host]);
    (source, record)
}

use crate::delivery::{object_store::transport::*, recovery::*};
use async_trait::async_trait;
use culprit_core::Result;
use std::{
    collections::BTreeMap,
    path::Path,
    pin::Pin,
    sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    },
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
                Fault::LateError => {
                    return Poll::Ready(Err(std::io::Error::other("owned late failure")));
                }
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
    pub object_reads: AtomicUsize,
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
        Self::try_capture(destination, schema, source, records, sequence).unwrap()
    }
    pub fn try_capture(
        destination: &str,
        schema: &str,
        source: CollectionDescription,
        records: Vec<TelemetryRecord>,
        sequence: u64,
    ) -> Result<Self> {
        let (mut manifest, _) = fixture();
        manifest.source = source;
        manifest.capture.observation_id = format!("native-observation-{sequence}");
        manifest.capture.revision.as_mut().unwrap().sequence = sequence.to_string();
        manifest.contract = Contract::new("pipeline".into(), "shard-0".into(), destination.into(), schema.into())?;
        manifest.range_id = digest(&serde_json::to_vec(&(&manifest.contract, &manifest.capture)).unwrap());
        let mut references = crate::collection_references::ObservationReferences::default();
        for record in &records {
            references.record(record);
        }
        manifest.reference_coverage = references.coverage(true, Some(&manifest.source));
        let mut objects = BTreeMap::new();
        manifest.batches = manifest
            .source
            .observation
            .collection()
            .streams()
            .iter()
            .map(|stream| -> Result<BatchManifest> {
                let mut bytes = Vec::new();
                let mut rows = Vec::new();
                for record in records.iter().filter(|record| record.stream() == *stream) {
                    let exported = ExportedRecord::new(&manifest.source, &manifest.capture, record.clone())?;
                    rows.push(RowReceipt::from_export(&exported, u32::try_from(rows.len() + 1).unwrap())?);
                    serde_json::to_writer(&mut bytes, &exported).map_err(|_| DeliveryProblem::Integrity.error())?;
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
                    delivery_required: stream.delivery_required(),
                };
                objects.insert(object, bytes);
                Ok(batch)
            })
            .collect::<Result<Vec<_>>>()?;
        manifest.validate()?;
        Ok(Self { manifest, objects, fault: Mutex::new(Fault::None), object_reads: AtomicUsize::new(0) })
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
    async fn pending_diagnostic_inputs(&self, _: Option<&str>, _: u32) -> Result<RecoveryPage> {
        unreachable!("not a diagnostic recovery enumeration fixture")
    }
    async fn diagnostic_input_pending(&self, _: &DiagnosticCaptureRef, _: u32) -> Result<bool> {
        Err(DeliveryProblem::UnsupportedSchema.error())
    }
    async fn retire(&self, _: &Fence, _: &crate::delivery::sink::PublishedRange) -> Result<()> {
        unreachable!("not a retirement fixture")
    }
    async fn capture_manifest(&self, capture: &CaptureIdentity) -> Result<RangeManifest> {
        if &self.manifest.capture != capture {
            return Err(DeliveryProblem::NotReady.error());
        }
        Ok(self.manifest.clone())
    }
    async fn diagnostic_heads(&self, _: Option<&str>, _: u32) -> Result<crate::delivery::recovery::DiagnosticHeadPage> {
        unreachable!("not a checkpoint enumeration fixture")
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
        self.object_reads.fetch_add(1, Ordering::SeqCst);
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
impl ObjectTransport for Blobs {
    async fn get(&self, key: &str) -> Result<Option<ObjectRead>> {
        Ok(self.objects.lock().unwrap().get(key).cloned().map(|bytes| ObjectRead {
            etag: "fixture".into(),
            bytes: bytes.len() as u64,
            body: Box::pin(std::io::Cursor::new(bytes)),
        }))
    }
    async fn put(&self, key: &str, bytes: Vec<u8>, condition: WriteCondition<'_>) -> Result<ObjectWrite> {
        assert!(matches!(condition, WriteCondition::Absent));
        let mut objects = self.objects.lock().unwrap();
        if objects.contains_key(key) {
            return Ok(ObjectWrite::ConditionNotMet);
        }
        objects.insert(key.into(), bytes);
        Ok(ObjectWrite::Written)
    }
    async fn put_file(&self, key: &str, path: &Path, bytes: u64) -> Result<ObjectWrite> {
        self.uploads.lock().unwrap().push(key.into());
        let body = std::fs::read(path).unwrap();
        assert_eq!(body.len() as u64, bytes);
        self.put(key, body, WriteCondition::Absent).await
    }
}

pub fn interval_queries(destination: &str, schema: &str) -> Recovery {
    let source = queries::source();
    let identity =
        NativeQueryIdentity { query_id: "opaque-query".into(), qualifiers: [("userid".into(), "17".into())].into() };
    let references = StatementReferences {
        query: Some(NativeDefinitionReference {
            native: identity.native(),
            definition_id: Some(identity.definition_id(&source).unwrap()),
        }),
        plan: None,
    };
    let runtime = TelemetryRecord::QueryRuntime(RuntimeMeasurement {
        key: NativeMeasurementKey {
            cohort: None,
            native: NativeIdentity::new("runtime-1"),
            references: references.clone(),
        },
        executions: "9007199254740993".into(),
        metrics: vec![
            RuntimeMetric::average(RuntimeMetricKey::Duration, RuntimeMetricUnit::Milliseconds, 1.25),
            RuntimeMetric::average(
                RuntimeMetricKey::LogicalReads,
                RuntimeMetricUnit::Pages { bytes_per_page: 16384 },
                7.0,
            ),
        ],
        page_server_reads: None,
        first_execution_at: None,
        last_execution_at: None,
    });
    let waits = TelemetryRecord::QueryWaits(WaitMeasurement {
        key: NativeMeasurementKey { cohort: None, native: NativeIdentity::new("wait-1"), references },
        category: "native-io".into(),
        unit: RuntimeMetricUnit::Microseconds,
        total: MeasurementValue::Real(1.25),
        average: None,
        minimum: None,
        maximum: None,
        last: None,
        standard_deviation: None,
        category_description: None,
        outcome_description: None,
    });
    let query = TelemetryRecord::QueryDefinition(QueryDefinition {
        variant_family: None,
        id: identity.definition_id(&source).unwrap(),
        identity,
        origin: StatementOrigin::Application,
        monitoring_template_id: None,
        context: Default::default(),
        context_available: false,
        source_text_bytes: None,
        sql: DefinitionPayload::Unavailable { reason: PayloadUnavailable::NotCollected },
    });
    Recovery::capture(destination, schema, source, vec![runtime, waits, query], 9)
}

pub fn cumulative_queries(destination: &str, schema: &str) -> Recovery {
    let (mut manifest, _) = fixture();
    let CollectionTarget::Database(database) = &mut manifest.source.target else { panic!("database fixture") };
    database.engine = culprit_core::Engine::Postgres;
    manifest.source.source = "statement-counters-fixture".into();
    manifest.source.observation = SourceObservation::Snapshot(StateSnapshotDescription {
        dataset: SnapshotDataset::StatementCounters,
        observed_from: "2020-01-01T12:00:00Z".into(),
        observed_until: "2020-01-01T12:00:01Z".into(),
        records: 1,
        sources: vec![StateSourceCoverage::collected("native-query-counters".into(), 1)],
    });
    let sample = StatementCounterSample {
        member: None,
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

pub fn query_compilation(destination: &str, schema: &str) -> Recovery {
    let (source, records) = query_compilation_evidence();
    Recovery::capture(destination, schema, source, records, 20_000)
}

pub fn query_compilation_evidence() -> (CollectionDescription, Vec<TelemetryRecord>) {
    let (mut manifest, _) = fixture();
    let source = culprit_core::source_id::QUERY_COMPILATION;
    manifest.source.source = "query-compilation-fixture".into();
    manifest.source.observation = SourceObservation::Snapshot(StateSnapshotDescription {
        dataset: SnapshotDataset::QueryCompilation,
        observed_from: "2020-01-01T12:00:00Z".into(),
        observed_until: "2020-01-01T12:00:01Z".into(),
        records: 1,
        sources: vec![StateSourceCoverage::collected(source.into(), 1)],
    });
    let compilation = MssqlQueryCompilation {
        statement: DefinitionPayload::Unavailable { reason: PayloadUnavailable::NotCollected },
        origin: StatementOrigin::Unknown,
        identity: NativeQueryIdentity {
            query_id: "42".into(),
            qualifiers: [
                ("context_settings_id".into(), "5".into()),
                ("object_id".into(), "0".into()),
                ("query_hash".into(), "0123456789ABCDEF".into()),
                ("initial_compile_at".into(), "2020-01-01T06:00:00.153333300Z".into()),
            ]
            .into(),
        },
        last_compile_at: "2020-01-01T11:59:00.1533333Z".into(),
        compilations: "9007199254740993".into(),
        average_duration_us: 1500.5,
        timings: MssqlCompilationTimings {
            last_duration_us: Some("9007199254740993".into()),
            binding: MssqlCompilationStage {
                average_duration_us: Some(25.5),
                last_duration_us: Some("0".into()),
                average_cpu_us: None,
                last_cpu_us: Some("9007199254740994".into()),
            },
            optimization: MssqlCompilationStage {
                average_duration_us: Some(1000.25),
                last_duration_us: Some("2800".into()),
                average_cpu_us: Some(0.0),
                last_cpu_us: Some("900".into()),
            },
        },
        memory: Some(culprit_core::CompilationMemory {
            average_kb: Some(32768.5),
            last_kb: Some(16384.0),
            maximum_kb: Some(65536.0),
        }),
        seconds_since_first_compile: 21600,
        reset: culprit_core::ResetMarker::Unknown,
    };
    let record = TelemetryRecord::Operational(OperationalObservation {
        source: source.into(),
        key: compilation.key().unwrap(),
        fact: OperationalFact::MssqlQueryCompilation(Box::new(compilation)),
    });
    (manifest.source, vec![record])
}

/// Models a scan retaining its already-acquired exact definition, not a standalone counter reader fetching SQL.
pub fn query_compilation_policy_evidence() -> (CollectionDescription, Vec<TelemetryRecord>) {
    let (source, mut records) = query_compilation_evidence();
    let TelemetryRecord::Operational(OperationalObservation {
        fact: OperationalFact::MssqlQueryCompilation(native),
        ..
    }) = &mut records[0]
    else {
        unreachable!()
    };
    native.statement = DefinitionPayload::sql(
        &format!(
            "SELECT {} FROM dbo.orders WHERE token='private-compilation-value' OPTION (RECOMPILE)",
            vec!["[field]"; 300].join(", ")
        ),
        culprit_core::Engine::Mssql,
    );
    native.origin = StatementOrigin::Application;
    native.compilations = "12".into();
    native.average_duration_us = 500_000.0;
    (source, records)
}

pub fn native_counter_sets() -> (CollectionDescription, Vec<TelemetryRecord>) {
    let (mut manifest, _) = fixture();
    manifest.source.target = CollectionTarget::Shared(SharedResourceIdentity {
        engine: culprit_core::Engine::Mssql,
        kind: SharedResourceKind::Instance,
        namespace: NativeIdentity::new("fixture-lineage"),
        native: NativeIdentity::new("fixture-instance"),
    });
    manifest.source.source = "native-instance-state".into();
    let source = "native-memory-clerks";
    let records = (0..2)
        .map(|node| {
            let mut cumulative = NativeCounterSample::gauge(
                "allocations",
                9_007_199_254_740_993,
                CounterUnit::Count,
                culprit_core::MeasurementScope::Server,
                source,
                "allocations",
            );
            cumulative.nature = CounterNature::Cumulative;
            cumulative.reset = culprit_core::ResetMarker::At("2020-01-01T01:00:00.000000001Z".into());
            let set = NativeCounterSet {
                ratios: Vec::new(),
                ratio_gaps: Vec::new(),
                identity: NativeIdentity::new("native-clerk")
                    .qualified("memory_node_id", node.to_string())
                    .qualified("incarnation", "native-generation"),
                scope: culprit_core::MeasurementScope::Server,
                counters: vec![
                    NativeCounterSample::gauge(
                        "pages_kb",
                        0,
                        CounterUnit::Kibibytes,
                        culprit_core::MeasurementScope::Server,
                        source,
                        "pages_kb",
                    ),
                    cumulative,
                ],
                properties: [
                    ("native_nulls".into(), "committed_kb".into()),
                    ("unavailable_columns".into(), "target_kb".into()),
                ]
                .into(),
            };
            TelemetryRecord::Operational(OperationalObservation {
                source: source.into(),
                key: set.key().unwrap(),
                fact: OperationalFact::CounterSet(Box::new(set)),
            })
        })
        .collect::<Vec<_>>();
    manifest.source.observation = SourceObservation::Snapshot(StateSnapshotDescription {
        dataset: SnapshotDataset::Operational,
        observed_from: "2020-01-01T12:00:00Z".into(),
        observed_until: "2020-01-01T12:00:01Z".into(),
        records: records.len() as u32,
        sources: vec![StateSourceCoverage::collected(source.into(), records.len() as u32)],
    });
    (manifest.source, records)
}

pub fn counter_sets(destination: &str, schema: &str) -> Recovery {
    let (source, records) = native_counter_sets();
    Recovery::capture(destination, schema, source, records, 30_000)
}

pub fn shared_operational() -> (CollectionDescription, Vec<TelemetryRecord>) {
    let (mut source, mut records) = native_counter_sets();
    records.push(TelemetryRecord::Operational(OperationalObservation {
        source: "native-performance-counters".into(),
        key: "connections".into(),
        fact: OperationalFact::Counter(NativeCounterSample::gauge(
            "connections",
            0,
            CounterUnit::Count,
            culprit_core::MeasurementScope::Server,
            "native-instance",
            "connections",
        )),
    }));
    records.push(TelemetryRecord::Operational(OperationalObservation {
        source: "native-tempdb-space".into(),
        key: "tempdb:data".into(),
        fact: OperationalFact::Space(OperationalSpace {
            kind: culprit_core::FileKind::Temp,
            population: StoragePopulation::SharedTempdbDataFiles,
            observed_at: "2020-01-01T12:00:00Z".into(),
            database: None,
            scope: culprit_core::FileScope::Server,
            native_name: Some("tempdb".into()),
            total_bytes: Some("1024".into()),
            used_bytes: None,
            limit_bytes: None,
            growth_enabled: Some(true),
            reuse_wait: None,
            parts: vec![
                SpaceAllocation { kind: SpaceAllocationKind::InternalObjects, bytes: Some("0".into()) },
                SpaceAllocation { kind: SpaceAllocationKind::VersionStore, bytes: None },
            ],
        }),
    }));
    let SourceObservation::Snapshot(snapshot) = &mut source.observation else { unreachable!() };
    snapshot.records = records.len() as u32;
    snapshot.sources.extend([
        StateSourceCoverage::collected("native-performance-counters".into(), 1),
        StateSourceCoverage::collected("native-tempdb-space".into(), 1),
    ]);
    (source, records)
}

pub fn shared_operational_capture(destination: &str, schema: &str) -> Recovery {
    let (source, records) = shared_operational();
    Recovery::capture(destination, schema, source, records, 30_001)
}

fn activity_sample() -> ActivityObservation {
    ActivityObservation {
        session_context: None,
        blocking_status: culprit_core::telemetry::ActivityBlockingStatus::Known,
        waiting_on_lock: None,
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
        open_transactions: Some(0),
        elapsed_ms: Some("120000".into()),
        transaction_age_ms: None,
        session_counters: ActivityCounters::default(),
        request_counters: Some(ActivityCounters::default()),
        wait: None,
        query_hash: None,
        statement_identity: None,
        client: None,
        statement: DefinitionPayload::Unavailable { reason: PayloadUnavailable::NotCollected },
    }
}

pub fn activity_with_wait_resource(
    destination: &str,
    schema: &str,
    sequence: u64,
    kind: culprit_core::WaitResourceKind,
) -> Recovery {
    let (mut source, _) = activity_evidence_cases().remove(0);
    let SourceObservation::Activity(snapshot) = &mut source.observation else { unreachable!() };
    snapshot.native_rows = 1;
    snapshot.observations = 1;
    snapshot.coverage = StateSourceCoverage::collected(source.source.clone(), 1);
    let mut row = activity_sample();
    row.state = "waiting".into();
    row.wait = Some(ActivityWait {
        class: None,
        native_type: Some(
            match kind {
                culprit_core::WaitResourceKind::MemoryPage => "PAGELATCH_EX",
                culprit_core::WaitResourceKind::PageIo => "PAGEIOLATCH_SH",
                culprit_core::WaitResourceKind::DataLock => "native-data-lock",
            }
            .into(),
        ),
        elapsed_ms: Some("40000".into()),
        owners: vec![],
        resources: None,
        identified_resources: vec![culprit_core::ObservedWaitResource {
            kind,
            native: NativeIdentity::new("native-wait-resource").qualified("namespace", "native-local"),
        }],
    });
    Recovery::capture(destination, schema, source, vec![TelemetryRecord::from(row)], sequence)
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
        mechanism: None,
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

mod page_server_definitions;
pub use page_server_definitions::{QueryStoreStatementCase, query_store_statement_evidence_cases};

mod storage;
pub use storage::{StorageCase, storage_evidence};
mod qualification;
pub use qualification::{QualificationFixture, QualificationFixtureCase};

mod instance_resources;
pub use instance_resources::{
    InstanceResourceCase, ProcessCpuCase, SchedulerCase, instance_resource_cpu_evidence, instance_resource_evidence,
    instance_resource_scheduler_evidence,
};

mod boxed_query_history;
pub use boxed_query_history::boxed_query_history_evidence;

mod group_replication;
pub use group_replication::{GroupReplicationCase, group_replication_evidence};

mod galera;
pub use galera::{GaleraCase, galera_evidence};

mod pool_settings;
pub use pool_settings::{PoolSettingsCase, assert_pool_settings_capture, pool_settings_evidence};

mod managed_instance_resources;
pub use managed_instance_resources::{
    ManagedInstanceResourceCase, managed_instance_resource_case, managed_instance_resource_evidence,
};
