//! One real capture owner for local-original consumer conformance. It does not contact a database.
use super::*;
use crate::{
    capture::{self, CollectionCancellation, SourceFactory},
    outbox::Outbox,
};
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

/// The source closes before this fixture exposes a durable result; retaining the fixture retains its temporary files.
pub struct LocalCaptureFixture {
    pub outbox: Option<Outbox>,
    pub directory: PathBuf,
    pub capture_directory: PathBuf,
    pub manifest: CollectionManifest,
    _temporary: tempfile::TempDir,
}

/// Selector names are conformance inputs, not provider selection or native schema interpretation.
pub struct LocalSnapshotCase {
    pub analysis: &'static str,
    pub source: CollectionDescription,
    pub records: Vec<TelemetryRecord>,
}
pub fn local_snapshot_evidence_cases() -> Vec<LocalSnapshotCase> {
    let case = |analysis, (source, records)| LocalSnapshotCase { analysis, source, records };
    vec![
        case("performance_counters", super::performance_counter_evidence(super::PerformanceCounterCase::Exact)),
        case(
            "tempdb_metadata_memory",
            super::tempdb_metadata_memory_evidence(super::TempdbMetadataMemoryCase::Reserved),
        ),
        case("table_memory", super::table_memory_evidence(super::TableMemoryCase::Exact)),
        case("full_text_populations", super::full_text_population_evidence(super::FullTextPopulationCase::Processing)),
        case("instance_resources", super::instance_resource_scheduler_evidence(super::SchedulerCase::Exact)),
        case("database_properties", super::database_properties_evidence(super::DatabasePropertiesCase::Exact)),
        case("table_full_text", super::table_full_text_evidence(super::TableFullTextCase::Enabled)),
        case("compilation", super::query_compilation_evidence()),
        case("configured_log_rates", super::configured_log_rate_evidence_cases().remove(0)),
        case("connection_outcomes", super::connection_outcome_evidence_cases().remove(0)),
        case("jobs", super::jobs_evidence(super::JobsCase::Failed)),
        case("cdc", super::cdc_evidence(super::CdcCase::ErrorAndProgress)),
        case("columnstore_rowgroups", super::columnstore_rowgroup_evidence(super::ColumnstoreRowgroupCase::States)),
        case("suspect_pages", super::suspect_page_history_evidence(super::SuspectPageHistoryCase::RepairedOnly)),
        case("backup_history", super::backup_history_evidence(super::BackupHistoryCase::MultipleRecords)),
        case("request_memory", super::request_memory_evidence_cases().remove(0)),
        case("task_space", super::task_space_evidence_cases().remove(0)),
        case("current_request_work", super::current_request_work_evidence_cases().remove(0)),
        case("statement_work", super::statement_work_evidence_cases().remove(0)),
        case("maintenance_progress", super::maintenance_progress_evidence_cases().remove(0)),
        case(
            "completed_spills",
            super::completed_spill_evidence_cases()
                .into_iter()
                .find(|case| case.name == "recorded_spills")
                .unwrap()
                .runtime,
        ),
        case("storage", super::storage_evidence(super::StorageCase::Exact)),
        case("pool_settings", super::pool_settings_evidence(super::PoolSettingsCase::Exact)),
        case("replication", super::replication_evidence(super::ReplicationCase::MssqlHyperscale)),
        case("group_replication", super::group_replication_evidence(super::GroupReplicationCase::Queued)),
        case("galera", super::galera_evidence(super::GaleraCase::Queued)),
    ]
}
impl LocalCaptureFixture {
    pub async fn query_history(source: CollectionDescription, records: Vec<TelemetryRecord>) -> Self {
        assert!(source.query_history().is_some());
        Self::with_shutdown(source, records, SourceShutdown::Quiescent).await
    }

    pub async fn snapshot(source: CollectionDescription, records: Vec<TelemetryRecord>) -> Self {
        assert!(source.observation.snapshot().is_some());
        Self::with_shutdown(source, records, SourceShutdown::Quiescent).await
    }

    pub async fn activity(source: CollectionDescription, records: Vec<TelemetryRecord>) -> Self {
        assert!(source.observation.activity().is_some());
        Self::with_shutdown(source, records, SourceShutdown::Quiescent).await
    }

    pub async fn with_shutdown(
        mut source: CollectionDescription,
        records: Vec<TelemetryRecord>,
        shutdown: SourceShutdown,
    ) -> Self {
        let temporary = tempfile::tempdir().unwrap();
        let directory = temporary.path().join("outbox");
        let mut outbox = Outbox::create(&directory, OutboxLimits::default()).unwrap();
        retain_focused_parent(&mut outbox, &mut source).await;
        let selection = selection(&source);
        let (capture_directory, manifest) = collect_original(&mut outbox, source, records, selection, shutdown).await;
        Self { outbox: Some(outbox), directory, capture_directory, manifest, _temporary: temporary }
    }

    pub fn original_bytes(&self) -> u64 {
        std::fs::metadata(self.capture_directory.join("manifest.json")).unwrap().len()
            + self.manifest.files.iter().map(|file| u64::from(file.bytes)).sum::<u64>()
    }
}

fn selection(source: &CollectionDescription) -> CollectionSelection {
    match &source.observation {
        SourceObservation::MssqlManagedInstanceResources(history) => {
            CollectionSelection::ResourceHistory { from: history.from.clone(), until: history.until.clone() }
        }
        SourceObservation::MssqlPoolSettings { .. } => CollectionSelection::ResourcePoolSettings,
        SourceObservation::Activity(_) => CollectionSelection::Activity,
        SourceObservation::TableMemory { target, .. } => CollectionSelection::TableMemory { target: target.clone() },
        SourceObservation::FullTextPopulations { target, .. } => {
            CollectionSelection::FullTextPopulations { target: target.clone() }
        }
        SourceObservation::ColumnstoreRowgroups { target, .. } => {
            CollectionSelection::ColumnstoreRowgroups { target: target.clone() }
        }
        SourceObservation::FocusedBlocking { result, .. } => {
            CollectionSelection::FocusedBlocking { focus: Box::new(result.focus.clone()) }
        }
        SourceObservation::FocusedRequestMemory { result, .. } => {
            CollectionSelection::FocusedRequestMemory { focus: Box::new(result.focus.clone()) }
        }
        SourceObservation::QueryHistory(history) => CollectionSelection::QueryHistory {
            from: history.interval.from.clone(),
            until: history.interval.until.clone(),
        },
        SourceObservation::MaintenanceProgress { result, .. } => CollectionSelection::MaintenanceProgress {
            focus: result.focus.clone().expect("focused native maintenance fixture"),
        },
        SourceObservation::CurrentRequestWork { result, .. } => CollectionSelection::CurrentRequestWork {
            focus: result.focus.clone().expect("focused native current-work fixture"),
        },
        observation if observation.snapshot().is_some() => {
            CollectionSelection::Snapshot { dataset: observation.snapshot().unwrap().dataset }
        }
        _ => panic!("local fixture requires an original history, Activity or snapshot read"),
    }
}
async fn collect_original(
    outbox: &mut Outbox,
    source: CollectionDescription,
    records: Vec<TelemetryRecord>,
    selection: CollectionSelection,
    shutdown: SourceShutdown,
) -> (PathBuf, CollectionManifest) {
    let request = CollectionRequest {
        subject: match &source.target {
            CollectionTarget::Database(_) => CollectionSubject::Database,
            CollectionTarget::Shared(resource) => CollectionSubject::Shared { resource: resource.clone() },
        },
        selection,
        budget: CollectionBudget::default(),
    };
    let closed = Arc::new(AtomicBool::new(false));
    let factory = OriginalSource { source, records, shutdown, closed: closed.clone() };
    let mut lease = outbox.reserve("local-evidence", &request).await.unwrap();
    let capture_directory = lease.directory().to_owned();
    let manifest = capture::collect(
        &culprit_read::admission::SourceGate::default(),
        &factory,
        request,
        &capture_directory,
        lease.identity().clone(),
        &CollectionCancellation::default(),
        Some(&mut lease),
    )
    .await
    .unwrap();
    assert!(closed.load(Ordering::SeqCst));
    lease.finish().await.unwrap();
    (capture_directory, manifest)
}

/// Controlled focused rows acquire their real prerequisite before the selected capture is sealed. This is a
/// local ownership fixture, not native execution/cost qualification or a replacement source of actor identity.
async fn retain_focused_parent(outbox: &mut Outbox, source: &mut CollectionDescription) {
    let focus = match &mut source.observation {
        SourceObservation::FocusedBlocking { result, .. } => &mut result.focus,
        SourceObservation::FocusedRequestMemory { result, .. } => &mut result.focus,
        SourceObservation::MaintenanceProgress { result, .. }
        | SourceObservation::CurrentRequestWork { result, .. } => {
            result.focus.as_mut().expect("focused fixture retains its original actor")
        }
        _ => return,
    };
    let original = CollectionDescription {
        target: focus.target.clone(),
        partition: focus.partition.clone(),
        source: focus.source.clone(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::Activity(ActivitySnapshotDescription {
            epoch: Some(focus.epoch.clone()),
            database_writable: focus.database_writable,
            observed_from: focus.original_observed_at.clone(),
            observed_until: focus.original_observed_at.clone(),
            at: focus.original_observed_at.clone(),
            scope: culprit_core::MeasurementScope::Database,
            population: ActivityPopulation::ActiveRequestsOpenTransactionsAndBlockers,
            native_rows: 1,
            observations: 1,
            coverage: StateSourceCoverage::observed(focus.source.clone(), 1, false),
        }),
    };
    let (directory, manifest) = collect_original(
        outbox,
        original,
        vec![TelemetryRecord::from(focus.original.clone())],
        CollectionSelection::Activity,
        SourceShutdown::Quiescent,
    )
    .await;
    assert_eq!(manifest.status, CollectionStatus::Complete);
    let file = manifest.files.iter().find(|file| file.stream == TelemetryStream::ActivityObservations).unwrap();
    let rows = std::fs::read_to_string(directory.join(&file.file)).unwrap();
    let row: ExportedRecord = serde_json::from_str(rows.lines().next().unwrap()).unwrap();
    let source = manifest.description.as_ref().unwrap();
    let reference = DiagnosticEvidenceRef::from_export(
        source,
        &digest(&std::fs::read(directory.join("manifest.json")).unwrap()),
        &row,
        0,
    )
    .unwrap();
    let TelemetryRecord::Activity(original) = row.record else { panic!() };
    focus.parent = reference;
    focus.original = *original;
    focus.statement_identity = focus.original.statement_identity.clone().unwrap();
    assert!(focus.matches_parent(source, &focus.original, &focus.parent));
}

#[derive(Clone)]
struct OriginalSource {
    source: CollectionDescription,
    records: Vec<TelemetryRecord>,
    shutdown: SourceShutdown,
    closed: Arc<AtomicBool>,
}
#[async_trait::async_trait]
impl SourceFactory for OriginalSource {
    async fn open(&self, request: &CollectionRequest) -> culprit_core::Result<Box<dyn TelemetrySource>> {
        assert!(self.source.matches_request(request));
        Ok(Box::new(self.clone()))
    }
}
#[async_trait::async_trait]
impl TelemetrySource for OriginalSource {
    async fn describe(&mut self) -> culprit_core::Result<CollectionDescription> {
        Ok(self.source.clone())
    }
    fn query_count(&self) -> u32 {
        0
    }
    async fn read_page(
        &mut self,
        stream: TelemetryStream,
        cursor: Option<&str>,
    ) -> culprit_core::Result<CollectionRead> {
        assert!(cursor.is_none());
        Ok(CollectionRead::Page {
            records: self.records.iter().filter(|row| row.stream() == stream).cloned().collect(),
            next_cursor: None,
        })
    }
    async fn finish(&mut self) -> culprit_core::Result<SourceCompletion> {
        Ok(SourceCompletion::Verified)
    }
    async fn close(&mut self) -> SourceShutdown {
        self.closed.store(true, Ordering::SeqCst);
        self.shutdown
    }
}
