//! Synthetic native rows pass through the real capture, Outbox, scan and report owners; no database is contacted.
use super::*;
use culprit_collector::{
    analytical::test_support::cached_runtime_evidence_cases,
    capture::{self, CollectionCancellation, SourceFactory},
    outbox::{CompletedCaptureBudget, Outbox, OutboxDelivery},
};
use culprit_core::{CompletedSpillState, DatabaseLocation, SourceState, telemetry::*};
use culprit_engine::{CompletedCaptureAnalysis, CompletedCaptureScanInput};
use culprit_read::admission::SourceGate;

mod database_performance_counters;
mod full_text_populations;
mod hyperscale_log_components;
mod memory_clerks;
mod source_overhead;
mod table_memory;
mod tempdb_metadata_memory;

#[derive(Clone)]
struct CapturedRows {
    source: CollectionDescription,
    records: Vec<TelemetryRecord>,
}
#[async_trait]
impl SourceFactory for CapturedRows {
    async fn open(&self, request: &CollectionRequest) -> Result<Box<dyn TelemetrySource>> {
        assert!(self.source.matches_request(request));
        Ok(Box::new(self.clone()))
    }
}
#[async_trait]
impl TelemetrySource for CapturedRows {
    async fn describe(&mut self) -> Result<CollectionDescription> {
        Ok(self.source.clone())
    }
    fn query_count(&self) -> u32 {
        0
    }
    async fn read_page(&mut self, stream: TelemetryStream, cursor: Option<&str>) -> Result<CollectionRead> {
        assert_eq!(self.source.observation.collection().source_streams(), &[stream]);
        assert!(cursor.is_none());
        Ok(CollectionRead::Page { records: std::mem::take(&mut self.records), next_cursor: None })
    }
    async fn finish(&mut self) -> Result<SourceCompletion> {
        assert!(self.records.is_empty());
        Ok(SourceCompletion::Verified)
    }
    async fn close(&mut self) -> SourceShutdown {
        SourceShutdown::Quiescent
    }
}

struct Captured {
    _home: tempfile::TempDir,
    outbox: Outbox,
    directory: std::path::PathBuf,
    manifest: CollectionManifest,
    attempt: DiagnosticAttemptReceipt,
    rows: Vec<TelemetryRecord>,
}
impl Captured {
    async fn new(partial: bool) -> Self {
        Self::with_incarnation(partial, Some("controlled-native-database-guid")).await
    }
    async fn with_incarnation(partial: bool, incarnation: Option<&str>) -> Self {
        let (mut source, rows) = cached_runtime_evidence_cases().pop().unwrap();
        let mut database = snapshots().requested.identity.unwrap();
        database.incarnation = incarnation.map(str::to_owned);
        source.target = database.into();
        let SourceObservation::Snapshot(header) = &mut source.observation else { panic!() };
        header.sources[0] = StateSourceCoverage::observed(header.sources[0].source.clone(), 1, !partial);
        Self::from_source("shop", source, rows).await
    }
    async fn from_source(connection: &str, source: CollectionDescription, rows: Vec<TelemetryRecord>) -> Self {
        let home = tempfile::tempdir().unwrap();
        let mut outbox = Outbox::create(&home.path().join("outbox"), Default::default()).unwrap();
        let subject = match &source.target {
            CollectionTarget::Database(_) => CollectionSubject::Database,
            CollectionTarget::Shared(resource) => CollectionSubject::Shared { resource: resource.clone() },
        };
        let request = CollectionRequest {
            subject,
            selection: match &source.observation {
                history if history.collection() == TelemetryCollection::ResourceHistory => {
                    let TelemetryRange::TimeWindow { from, until } = history.range().unwrap() else {
                        panic!("history fixture requires native time bounds")
                    };
                    CollectionSelection::ResourceHistory { from, until }
                }
                SourceObservation::FullTextPopulations { target, .. } => {
                    CollectionSelection::FullTextPopulations { target: target.clone() }
                }
                SourceObservation::TableMemory { target, .. } => {
                    CollectionSelection::TableMemory { target: target.clone() }
                }
                _ => CollectionSelection::Snapshot { dataset: source.observation.snapshot().unwrap().dataset },
            },
            budget: CollectionBudget::default(),
        };
        let mut lease = outbox.reserve(connection, &request).await.unwrap();
        let directory = lease.directory().to_owned();
        let identity = lease.identity().clone();
        let source = CapturedRows { source, records: rows.clone() };
        let manifest = capture::collect(
            &SourceGate::default(),
            &source,
            request,
            &directory,
            identity,
            &CollectionCancellation::default(),
            Some(&mut lease),
        )
        .await
        .unwrap();
        assert_eq!(manifest.status, CollectionStatus::Complete);
        lease.finish().await.unwrap();
        let attempt = outbox.attempt_receipt(&manifest.capture, None).await.unwrap();
        Self { _home: home, outbox, directory, manifest, attempt, rows }
    }
    fn input<'a>(
        &'a self,
        delivery: &'a OutboxDelivery,
        cancellation: &'a CollectionCancellation,
    ) -> CompletedCaptureScanInput<'a> {
        CompletedCaptureScanInput {
            delivery,
            analysis: CompletedCaptureAnalysis::CachedRuntime,
            attempt: &self.attempt,
            budget: CompletedCaptureBudget::default(),
            cancellation,
        }
    }
    fn source(&self) -> &CollectionDescription {
        self.manifest.description.as_ref().unwrap()
    }
    fn reference(&self) -> DiagnosticEvidenceRef {
        let row = ExportedRecord::new(self.source(), &self.attempt.capture, self.rows[0].clone()).unwrap();
        DiagnosticEvidenceRef::from_export(self.source(), self.attempt.manifest_sha256.as_deref().unwrap(), &row, 0)
            .unwrap()
    }
    fn batch(&self) -> PopulationSnapshots {
        let mut batch = snapshots();
        let database = self.source().target.database().unwrap();
        for snapshot in std::iter::once(&mut batch.requested).chain(batch.populations.iter_mut()) {
            snapshot.bind_identity(database.clone());
        }
        batch
    }
    fn body(&self) -> std::path::PathBuf {
        self.directory.join(&self.manifest.files[0].file)
    }
}

fn assert_captured(report: &culprit_core::Report, original: &Captured) {
    let read = report.snapshot.operations.cached_runtime.as_deref().unwrap();
    assert!(matches!(read.origin, CachedRuntimeReadOrigin::Captured { .. }));
    assert_eq!(read.description, *original.source());
    assert_eq!(read.captured_references().unwrap(), Some(vec![original.reference()]));
    let view = report.completed_spills.as_deref().unwrap();
    assert_eq!(view.entries[0].observation.captured(), Some(&original.reference()));
    assert_eq!(view.entries[0].state, CompletedSpillState::RecordedSpills);
    assert_eq!(view.entries[0].metrics[0].text.as_deref(), Some("9007199254740993"));
    assert_eq!(view.context.observed_from, "2020-01-01T12:00:00Z");
    assert_ne!(view.context.observed_from, report.snapshot.taken_at);
    let retained = report.evidence.as_ref().unwrap().collected.as_ref().unwrap();
    assert_eq!(retained.operations.cached_runtime.as_deref(), Some(read));
}

#[tokio::test]
async fn a_ready_original_capture_is_authenticated_before_one_normal_scan_and_survives_saved_report_replay() {
    let captured = Captured::new(false).await;
    let (_home, workspace, state) = workspace(captured.batch(), SourceShutdown::Quiescent);
    let report = workspace
        .scan_with_completed_capture(
            "shop",
            &options(),
            captured.input(&captured.outbox.delivery(), &CollectionCancellation::default()),
        )
        .await
        .unwrap();
    assert_captured(&report, &captured);
    assert_eq!(state.acquisitions.load(Ordering::SeqCst), 1);
    assert_eq!(state.deadlocks.load(Ordering::SeqCst), 1);
    assert_eq!(state.events.load(Ordering::SeqCst), 1);
    assert_eq!(state.active.load(Ordering::SeqCst), 0);
    assert_eq!(state.analyzed.lock().unwrap().len(), 3, "existing owner checks run only after provider drop");
    let reopened = workspace.report_at("shop", &report.snapshot.id).unwrap();
    assert_captured(&reopened, &captured);
    assert_eq!(state.acquisitions.load(Ordering::SeqCst), 1, "replay opens no source");
    for population in ["1", "7"] {
        let selected =
            workspace.latest_population_report("shop", None, Some(&NativeIdentity::new(population))).unwrap().unwrap();
        assert!(selected.snapshot.operations.cached_runtime.is_none());
        assert!(selected.completed_spills.is_none());
    }
}

#[tokio::test]
async fn the_population_api_attaches_a_partial_native_population_once_without_copying_it_into_execution_groups() {
    let captured = Captured::new(true).await;
    let (_home, workspace, state) = workspace(captured.batch(), SourceShutdown::Quiescent);
    let reports = workspace
        .scan_populations_with_completed_capture(
            "shop",
            &options(),
            captured.input(&captured.outbox.delivery(), &CollectionCancellation::default()),
        )
        .await
        .unwrap();
    assert_captured(&reports.requested, &captured);
    let read = reports.requested.snapshot.operations.cached_runtime.as_deref().unwrap();
    assert!(read.read_complete);
    assert_eq!(read.description.observation.snapshot().unwrap().sources[0].state, SourceState::Partial);
    let view = reports.requested.completed_spills.as_deref().unwrap();
    assert!(view.read_complete);
    assert_eq!(view.sources[0].state, SourceState::Partial);
    assert_eq!(reports.populations.len(), 2);
    assert!(
        reports
            .populations
            .iter()
            .all(|report| report.snapshot.operations.cached_runtime.is_none() && report.completed_spills.is_none())
    );
    assert_eq!(state.acquisitions.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn a_proven_native_database_rename_and_endpoint_alias_preserve_the_original_captured_descriptor() {
    let captured = Captured::new(false).await;
    let mut batch = captured.batch();
    for snapshot in std::iter::once(&mut batch.requested).chain(batch.populations.iter_mut()) {
        let mut database = snapshot.identity.clone().unwrap();
        database.database = "renamed-shop".into();
        database.location =
            DatabaseLocation::Server { host: "new-native-route.invalid".into(), port: 1444, instance: None };
        snapshot.server.database = database.database.clone();
        snapshot.bind_identity(database);
    }
    let (_home, workspace, _) = workspace(batch, SourceShutdown::Quiescent);
    let report = workspace
        .scan_with_completed_capture(
            "shop",
            &options(),
            captured.input(&captured.outbox.delivery(), &CollectionCancellation::default()),
        )
        .await
        .unwrap();
    assert_eq!(report.snapshot.server.database, "renamed-shop");
    assert_captured(&report, &captured);
}

#[tokio::test]
async fn unknown_or_conflicting_native_database_authority_refuses_attachment_before_any_report_is_published() {
    for change in ["unknown-capture", "unknown-scan", "replacement", "missing", "engine", "identity-database"] {
        let captured = Captured::with_incarnation(
            false,
            (change != "unknown-capture").then_some("controlled-native-database-guid"),
        )
        .await;
        let mut batch = captured.batch();
        for snapshot in std::iter::once(&mut batch.requested).chain(batch.populations.iter_mut()) {
            match change {
                "unknown-scan" => snapshot.identity.as_mut().unwrap().incarnation = None,
                "replacement" => {
                    snapshot.identity.as_mut().unwrap().incarnation = Some("replacement-native-database-guid".into())
                }
                "missing" => snapshot.identity = None,
                "engine" => snapshot.identity.as_mut().unwrap().engine = Engine::Postgres,
                "identity-database" => snapshot.identity.as_mut().unwrap().database = "unrelated-native-label".into(),
                _ => {}
            }
        }
        let (_home, workspace, state) = workspace(batch, SourceShutdown::Quiescent);
        let selected = SnapshotOptions { workload_population: Some(NativeIdentity::new("7")), ..options() };
        let error = workspace
            .scan_with_completed_capture(
                "shop",
                &selected,
                captured.input(&captured.outbox.delivery(), &CollectionCancellation::default()),
            )
            .await
            .unwrap_err();
        assert_eq!(
            error.detail,
            Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::InvalidContract)),
            "{change}"
        );
        assert!(workspace.report_history("shop").unwrap().is_empty(), "{change}");
        assert_eq!(state.active.load(Ordering::SeqCst), 0, "the original scan owner is settled before refusal");
    }
}

#[tokio::test]
async fn an_existing_requested_read_is_never_overwritten_even_by_the_same_authenticated_capture() {
    let captured = Captured::new(false).await;
    let existing = captured
        .outbox
        .delivery()
        .completed_snapshot(
            &captured.attempt,
            SnapshotDataset::CachedRuntime,
            CompletedCaptureBudget::default(),
            &CollectionCancellation::default(),
        )
        .await
        .unwrap()
        .into_cached_runtime_read()
        .unwrap();
    let mut batch = captured.batch();
    batch.requested.operations.cached_runtime = Some(Box::new(existing));
    let (_home, workspace, state) = workspace(batch, SourceShutdown::Quiescent);
    let error = workspace
        .scan_with_completed_capture(
            "shop",
            &options(),
            captured.input(&captured.outbox.delivery(), &CollectionCancellation::default()),
        )
        .await
        .unwrap_err();
    assert_eq!(error.detail, Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::Conflict)));
    assert!(workspace.report_history("shop").unwrap().is_empty());
    assert_eq!(state.active.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn cancellation_tampering_and_local_byte_refusal_cannot_construct_a_scan_provider() {
    for refusal in ["cancelled", "body", "manifest", "bytes"] {
        let captured = Captured::new(false).await;
        let (_home, workspace, state) = workspace(captured.batch(), SourceShutdown::Quiescent);
        let cancellation = CollectionCancellation::default();
        if refusal == "cancelled" {
            cancellation.cancel();
        }
        if refusal == "body" {
            let original = std::fs::read_to_string(captured.body()).unwrap();
            let changed = original.replace("9007199254740993", "9007199254740994");
            assert_ne!(original, changed);
            std::fs::write(captured.body(), changed).unwrap();
        }
        let mut attempt = captured.attempt.clone();
        if refusal == "manifest" {
            attempt.manifest_sha256 = Some(digest(b"not-the-original-manifest"));
        }
        let delivery = captured.outbox.delivery();
        let budget = if refusal == "bytes" {
            CompletedCaptureBudget { max_bytes: 1, ..Default::default() }
        } else {
            CompletedCaptureBudget::default()
        };
        let error = workspace
            .scan_with_completed_capture(
                "shop",
                &options(),
                CompletedCaptureScanInput {
                    delivery: &delivery,
                    analysis: CompletedCaptureAnalysis::CachedRuntime,
                    attempt: &attempt,
                    budget,
                    cancellation: &cancellation,
                },
            )
            .await
            .unwrap_err();
        if refusal == "cancelled" {
            assert_eq!(CollectionStop::from_error(&error), CollectionStop::Cancelled);
        }
        assert_eq!(state.active.load(Ordering::SeqCst), 0, "{refusal}");
        assert_eq!(state.factories.load(Ordering::SeqCst), 0, "{refusal}");
        assert_eq!(state.acquisitions.load(Ordering::SeqCst), 0, "{refusal}");
        assert!(workspace.report_history("shop").unwrap().is_empty());
    }
}

#[tokio::test]
async fn unconfirmed_scan_shutdown_withholds_the_attached_original_and_all_report_populations() {
    let captured = Captured::new(false).await;
    let (_home, workspace, state) = workspace(captured.batch(), SourceShutdown::Unconfirmed);
    let error = workspace
        .scan_with_completed_capture(
            "shop",
            &options(),
            captured.input(&captured.outbox.delivery(), &CollectionCancellation::default()),
        )
        .await
        .unwrap_err();
    assert_eq!(CollectionStop::from_error(&error), CollectionStop::CancellationUnconfirmed);
    assert_eq!(state.active.load(Ordering::SeqCst), 0);
    assert!(state.analyzed.lock().unwrap().is_empty());
    assert!(workspace.report_history("shop").unwrap().is_empty());
    assert!(workspace.collection_admission_status().await.unwrap().source_quiescence_required);
}

#[tokio::test]
async fn dropping_a_native_scan_future_preserves_existing_ownership_debt_and_never_attaches_or_persists() {
    let captured = Captured::new(false).await;
    let (_home, workspace, state) = workspace(captured.batch(), SourceShutdown::Quiescent);
    state.block_snapshots.store(true, Ordering::SeqCst);
    let delivery = captured.outbox.delivery();
    let cancellation = CollectionCancellation::default();
    let selected_options = options();
    {
        let scanning =
            workspace.scan_with_completed_capture("shop", &selected_options, captured.input(&delivery, &cancellation));
        tokio::pin!(scanning);
        tokio::select! {
            result = &mut scanning => panic!("blocked native fixture completed: {result:?}"),
            _ = state.snapshot_entered.notified() => {},
        }
    }
    assert_eq!(state.active.load(Ordering::SeqCst), 0);
    assert!(state.analyzed.lock().unwrap().is_empty());
    assert!(workspace.report_history("shop").unwrap().is_empty());
    assert!(
        workspace.collection_admission_status().await.unwrap().source_quiescence_required,
        "dropping a scan is not proof of native shutdown"
    );
}

#[tokio::test]
async fn ordinary_scans_do_not_read_or_attach_existing_local_captures() {
    let captured = Captured::new(false).await;
    let (_home, workspace, state) = workspace(captured.batch(), SourceShutdown::Quiescent);
    std::fs::write(captured.body(), b"not an exported record\n").unwrap();
    let report = workspace.scan("shop", &options()).await.unwrap();
    assert!(report.snapshot.operations.cached_runtime.is_none());
    assert!(report.completed_spills.is_none());
    assert_eq!(state.acquisitions.load(Ordering::SeqCst), 1);
}

mod group_replication;

mod galera;

mod managed_instance_resources;

mod index_configuration;
