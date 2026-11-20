//! This is a local ownership/integration proof over controlled native-shaped facts, not native cost qualification.
mod hyperscale_log_components;
mod tempdb_metadata_memory;
use super::*;
use clap::Parser;
use culprit_collector::analytical::test_support::{LocalCaptureFixture, MemoryClerkCase, memory_clerk_evidence};
use culprit_core::{
    AuthKind, Engine, ExplainMode, LiveState, Plan, Provider, ServerInfo, Snapshot, SnapshotOptions,
    telemetry::{CollectionStop, SourceShutdown},
};
use culprit_engine::{LocalCaptureTask, NewConnection, ProviderRegistration};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use tokio::sync::Notify;

fn clerk_original(observation_id: &str) -> CapturedOriginal<'_> {
    CapturedOriginal { observation_id, analysis: LocalCaptureAnalysisKind::MemoryClerks }
}

fn args(values: &[&str]) -> std::result::Result<crate::ScanArgs, clap::Error> {
    let cli = crate::Cli::try_parse_from([&["culprit", "scan", "local-evidence"], values].concat())?;
    let crate::Command::Scan(args) = cli.command else { panic!("scan command") };
    Ok(args)
}

#[test]
fn attaching_originals_requires_both_explicit_flags_and_preserves_independent_scan_and_log_options() {
    assert!(args(&["--evidence-outbox", "outbox"]).is_err());
    assert!(args(&["--memory-clerks-capture", "observation"]).is_err());
    let parsed = args(&[
        "--evidence-outbox",
        "outbox",
        "--memory-clerks-capture",
        "observation",
        "--window",
        "6h",
        "--top",
        "12",
        "--plans",
        "3",
        "--query-log",
        "framework.jsonl",
    ])
    .unwrap();
    assert_eq!(parsed.captured.evidence_outbox, Some(PathBuf::from("outbox")));
    assert_eq!(parsed.captured.memory_clerks_capture.as_deref(), Some("observation"));
    assert_eq!(parsed.snapshot_options().window_minutes, 360);
    assert_eq!(parsed.snapshot_options().top_queries, 12);
    assert_eq!(parsed.snapshot_options().plans, 3);
    assert_eq!(parsed.query_log, Some(PathBuf::from("framework.jsonl")));
    assert!(!parsed.snapshot_options().collect_shared_resources, "attachment is not a collection opt-in");
    let ordinary = args(&[]).unwrap();
    assert!(ordinary.captured.evidence_outbox.is_none());
    assert!(ordinary.captured.memory_clerks_capture.is_none());
}

#[derive(Default)]
struct PendingScan {
    started: Notify,
    factories: AtomicUsize,
    polls: AtomicUsize,
    dropped: AtomicUsize,
}
struct PendingSnapshot(Arc<PendingScan>);
impl Drop for PendingSnapshot {
    fn drop(&mut self) {
        self.0.dropped.fetch_add(1, Ordering::SeqCst);
    }
}

struct ScanProvider(Arc<AtomicUsize>, Option<Arc<PendingScan>>, Option<&'static str>);
#[async_trait::async_trait]
impl Provider for ScanProvider {
    fn engine(&self) -> Engine {
        Engine::Mssql
    }
    fn source_shutdown(&self) -> SourceShutdown {
        SourceShutdown::Quiescent
    }
    async fn probe(&self) -> Result<ServerInfo> {
        Ok(serde_json::from_str::<Snapshot>(include_str!("../../../../fixtures/mssql-demo.json")).unwrap().server)
    }
    async fn snapshot(&self, _: &str, options: &SnapshotOptions) -> Result<Snapshot> {
        self.0.fetch_add(1, Ordering::SeqCst);
        if let Some(pending) = &self.1 {
            let _owner = PendingSnapshot(pending.clone());
            pending.started.notify_one();
            return std::future::poll_fn(|_| {
                pending.polls.fetch_add(1, Ordering::SeqCst);
                std::task::Poll::Pending
            })
            .await;
        }
        let mut snapshot: Snapshot =
            serde_json::from_str(include_str!("../../../../fixtures/mssql-demo.json")).unwrap();
        snapshot.options = Some(options.clone());
        // Only the new database-scoped control supplies this independent synthetic native identity.
        snapshot.identity.as_mut().unwrap().incarnation = self.2.map(str::to_owned);
        Ok(snapshot)
    }
    async fn explain(&self, _: &str, _: ExplainMode) -> Result<Plan> {
        panic!("no application execution")
    }
    async fn live(&self) -> Result<LiveState> {
        panic!("no activity acquisition")
    }
    async fn run(&self, _: &str, _: usize) -> Result<culprit_core::ResultSet> {
        panic!("no application execution")
    }
}
fn workspace(fixture: &LocalCaptureFixture, calls: Arc<AtomicUsize>, pending: Option<Arc<PendingScan>>) -> Workspace {
    workspace_with_incarnation(fixture, calls, pending, None)
}
fn workspace_with_incarnation(
    fixture: &LocalCaptureFixture,
    calls: Arc<AtomicUsize>,
    pending: Option<Arc<PendingScan>>,
    incarnation: Option<&'static str>,
) -> Workspace {
    let workspace = Workspace::open_in(fixture.directory.parent().unwrap().join("workspace")).with_provider(
        Engine::Mssql,
        ProviderRegistration::new(
            Arc::new(move |_| {
                if let Some(pending) = &pending {
                    pending.factories.fetch_add(1, Ordering::SeqCst);
                }
                Ok(Box::new(ScanProvider(calls.clone(), pending.clone(), incarnation)) as Box<dyn Provider>)
            }),
            vec![],
        )
        .unwrap(),
    );
    workspace
        .add_connection(NewConnection {
            name: "local-evidence".into(),
            input: "Server=fixture.invalid;Database=fixture".into(),
            engine: Some(Engine::Mssql),
            auth: Some(AuthKind::None),
            ..Default::default()
        })
        .unwrap();
    workspace
}

#[tokio::test]
async fn actual_cli_scan_attachment_saves_original_shared_capture_for_offline_default_host_reanalysis() {
    let (source, rows) = memory_clerk_evidence(MemoryClerkCase::SharedInstance);
    let mut fixture = LocalCaptureFixture::snapshot(source.clone(), rows).await;
    drop(fixture.outbox.take());
    let calls = Arc::new(AtomicUsize::new(0));
    let workspace = workspace(&fixture, calls.clone(), None);
    let parsed = args(&[
        "--evidence-outbox",
        fixture.directory.to_str().unwrap(),
        "--memory-clerks-capture",
        &fixture.manifest.capture.observation_id,
        "--window",
        "6h",
        "--plans",
        "0",
    ])
    .unwrap();
    let report = scan(&workspace, &parsed).await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(report.snapshot.options.as_ref().unwrap().window_minutes, 360);
    assert_eq!(report.snapshot.options.as_ref().unwrap().plans, 0);
    let original = report.snapshot.operations.memory_clerks.as_deref().unwrap();
    assert_eq!(original.description, source);
    assert_eq!(original.observations.len(), 3, "unrelated source body is retained too");
    assert_eq!(original.origin.capture.capture, fixture.manifest.capture);
    assert_eq!(report.memory_clerks.as_ref().unwrap().native_rows, Some(2));
    assert_eq!(
        report.memory_clerks.as_ref().unwrap().entries[0]
            .fields
            .iter()
            .find(|field| field.key == "pages_kb")
            .unwrap()
            .value
            .as_deref(),
        Some("9007199254740993")
    );
    let path = workspace.paths().reports_dir("local-evidence").join(format!("{}.json", report.snapshot.id));
    let no_source = Workspace::open_in(fixture.directory.parent().unwrap().join("offline-workspace")).with_provider(
        Engine::Mssql,
        ProviderRegistration::new(
            Arc::new(|_| panic!("saved report reanalysis must not construct a provider")),
            vec![],
        )
        .unwrap(),
    );
    let reopened = no_source.analyze_file(&path).unwrap();
    assert_eq!(reopened.snapshot.operations.memory_clerks.as_deref(), Some(original));
    assert_eq!(
        serde_json::to_value(&reopened.memory_clerks).unwrap(),
        serde_json::to_value(&report.memory_clerks).unwrap()
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn a_corrupt_local_capture_refuses_the_cli_action_before_the_normal_scan() {
    let (source, rows) = memory_clerk_evidence(MemoryClerkCase::SharedInstance);
    let mut fixture = LocalCaptureFixture::snapshot(source, rows).await;
    let file = fixture.capture_directory.join(&fixture.manifest.files[0].file);
    std::fs::write(file, b"{}\n").unwrap();
    drop(fixture.outbox.take());
    let calls = Arc::new(AtomicUsize::new(0));
    let workspace = workspace(&fixture, calls.clone(), None);
    let parsed = args(&[
        "--evidence-outbox",
        fixture.directory.to_str().unwrap(),
        "--memory-clerks-capture",
        &fixture.manifest.capture.observation_id,
    ])
    .unwrap();
    assert!(scan(&workspace, &parsed).await.is_err());
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert!(workspace.report_history("local-evidence").unwrap().is_empty());
}

struct AwaitedReader {
    started: Notify,
    release: Notify,
    settled: AtomicUsize,
}
impl LocalCaptureReader for AwaitedReader {
    fn timeout(&self) -> std::time::Duration {
        std::time::Duration::from_secs(5)
    }
    fn analyze<'a>(&'a self, request: LocalCaptureRequest<'a>) -> LocalCaptureTask<'a> {
        Box::pin(async move {
            self.started.notify_one();
            self.release.notified().await;
            assert!(request.operation.is_cancelled());
            self.settled.fetch_add(1, Ordering::SeqCst);
            Err(CollectionStop::CancellationUnconfirmed.error())
        })
    }
}

#[tokio::test]
async fn local_cli_cancellation_awaits_the_same_reader_and_preserves_unconfirmed_failure() {
    let (source, rows) = memory_clerk_evidence(MemoryClerkCase::SharedInstance);
    let fixture = LocalCaptureFixture::snapshot(source, rows).await;
    let workspace = workspace(&fixture, Arc::new(AtomicUsize::new(0)), None);
    let reader = AwaitedReader { started: Notify::new(), release: Notify::new(), settled: AtomicUsize::new(0) };
    let cancellation = CollectionCancellation::default();
    let reading = read(
        &workspace,
        &reader,
        "local-evidence",
        clerk_original(&fixture.manifest.capture.observation_id),
        &cancellation,
    );
    let signal = async {
        reader.started.notified().await;
        Ok(())
    };
    let stopping = crate::collection_shutdown::with_signal(&cancellation, reading, signal);
    tokio::pin!(stopping);
    assert!(tokio::time::timeout(std::time::Duration::from_millis(10), &mut stopping).await.is_err());
    assert!(cancellation.is_cancelled());
    assert_eq!(reader.settled.load(Ordering::SeqCst), 0);
    reader.release.notify_one();
    let error = stopping.await.unwrap_err();
    assert_eq!(CollectionStop::from_error(&error), CollectionStop::CancellationUnconfirmed);
    assert_eq!(reader.settled.load(Ordering::SeqCst), 1);
}

struct CompletedReader {
    result: Mutex<Option<LocalCapturedAnalysis>>,
    cancellation_on_completion: CollectionCancellation,
}
impl LocalCaptureReader for CompletedReader {
    fn timeout(&self) -> std::time::Duration {
        std::time::Duration::from_secs(5)
    }
    fn analyze<'a>(&'a self, _: LocalCaptureRequest<'a>) -> LocalCaptureTask<'a> {
        Box::pin(async move {
            self.cancellation_on_completion.cancel();
            Ok(self.result.lock().unwrap().take().unwrap())
        })
    }
}

async fn original(workspace: &Workspace, fixture: &LocalCaptureFixture) -> culprit_engine::LocalMemoryClerks {
    let reader =
        OutboxCaptureReader::delivery(fixture.outbox.as_ref().unwrap().delivery(), CompletedCaptureBudget::default())
            .unwrap();
    let captured = read(
        workspace,
        &reader,
        "local-evidence",
        clerk_original(&fixture.manifest.capture.observation_id),
        &CollectionCancellation::default(),
    )
    .await
    .unwrap();
    let LocalCapturedAnalysis::MemoryClerks(captured) = captured else { panic!("requested clerk original") };
    captured
}

#[tokio::test]
async fn simultaneous_local_success_and_cancellation_cannot_enter_the_scan_factory() {
    let (source, rows) = memory_clerk_evidence(MemoryClerkCase::SharedInstance);
    let fixture = LocalCaptureFixture::snapshot(source, rows).await;
    let pending = Arc::new(PendingScan::default());
    let workspace = workspace(&fixture, Arc::new(AtomicUsize::new(0)), Some(pending.clone()));
    let cancellation = CollectionCancellation::default();
    let reader = CompletedReader {
        result: Mutex::new(Some(LocalCapturedAnalysis::MemoryClerks(original(&workspace, &fixture).await))),
        cancellation_on_completion: cancellation.clone(),
    };
    let error = captured_scan(
        &workspace,
        &args(&[]).unwrap(),
        &reader,
        clerk_original(&fixture.manifest.capture.observation_id),
        &cancellation,
    )
    .await
    .unwrap_err();
    assert_eq!(CollectionStop::from_error(&error), CollectionStop::Cancelled);
    assert_eq!(pending.factories.load(Ordering::SeqCst), 0);
    assert!(workspace.report_history("local-evidence").unwrap().is_empty());
}

#[tokio::test]
async fn cancellation_between_the_local_read_and_dispatch_does_not_construct_the_scan_future() {
    let (source, rows) = memory_clerk_evidence(MemoryClerkCase::SharedInstance);
    let fixture = LocalCaptureFixture::snapshot(source, rows).await;
    let pending = Arc::new(PendingScan::default());
    let workspace = workspace(&fixture, Arc::new(AtomicUsize::new(0)), Some(pending.clone()));
    let captured = original(&workspace, &fixture).await;
    let cancellation = CollectionCancellation::default();
    cancellation.cancel();
    let mut constructed = false;
    let options = SnapshotOptions::default();
    let error = scan_until_cancelled(&cancellation, || {
        constructed = true;
        workspace.scan_with_captured_memory_clerks("local-evidence", &options, None, captured)
    })
    .await
    .unwrap_err();
    assert_eq!(CollectionStop::from_error(&error), CollectionStop::Cancelled);
    assert!(!constructed);
    assert_eq!(pending.factories.load(Ordering::SeqCst), 0);
    assert!(!workspace.collection_admission_status().await.unwrap().source_quiescence_required);
}

#[tokio::test]
async fn an_already_ready_signal_precedes_even_an_immediately_ready_local_original() {
    let (source, rows) = memory_clerk_evidence(MemoryClerkCase::SharedInstance);
    let fixture = LocalCaptureFixture::snapshot(source, rows).await;
    let pending = Arc::new(PendingScan::default());
    let workspace = workspace(&fixture, Arc::new(AtomicUsize::new(0)), Some(pending.clone()));
    let cancellation = CollectionCancellation::default();
    let reader = CompletedReader {
        result: Mutex::new(Some(LocalCapturedAnalysis::MemoryClerks(original(&workspace, &fixture).await))),
        cancellation_on_completion: CollectionCancellation::default(),
    };
    let parsed = args(&[]).unwrap();
    let action = captured_scan(
        &workspace,
        &parsed,
        &reader,
        clerk_original(&fixture.manifest.capture.observation_id),
        &cancellation,
    );
    let error = crate::collection_shutdown::with_signal(&cancellation, action, async { Ok(()) }).await.unwrap_err();
    assert_eq!(CollectionStop::from_error(&error), CollectionStop::Cancelled);
    assert_eq!(pending.factories.load(Ordering::SeqCst), 0);
    assert!(!workspace.collection_admission_status().await.unwrap().source_quiescence_required);
}

#[tokio::test]
async fn one_signal_owner_retires_the_same_pending_scan_and_keeps_durable_native_debt() {
    let (source, rows) = memory_clerk_evidence(MemoryClerkCase::SharedInstance);
    let fixture = LocalCaptureFixture::snapshot(source, rows).await;
    let pending = Arc::new(PendingScan::default());
    let calls = Arc::new(AtomicUsize::new(0));
    let workspace = workspace(&fixture, calls.clone(), Some(pending.clone()));
    let reader =
        OutboxCaptureReader::delivery(fixture.outbox.as_ref().unwrap().delivery(), CompletedCaptureBudget::default())
            .unwrap();
    let cancellation = CollectionCancellation::default();
    let parsed = args(&[]).unwrap();
    let action = captured_scan(
        &workspace,
        &parsed,
        &reader,
        clerk_original(&fixture.manifest.capture.observation_id),
        &cancellation,
    );
    let signal = async {
        pending.started.notified().await;
        Ok(())
    };
    let error = crate::collection_shutdown::with_signal(&cancellation, action, signal).await.unwrap_err();
    assert_eq!(CollectionStop::from_error(&error), CollectionStop::CancellationUnconfirmed);
    assert_eq!(pending.dropped.load(Ordering::SeqCst), 1);
    assert_eq!(
        pending.polls.load(Ordering::SeqCst),
        1,
        "acknowledged cancellation must not poll native follow-up work"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(workspace.collection_admission_status().await.unwrap().source_quiescence_required);
    assert!(workspace.scan("local-evidence", &SnapshotOptions::default()).await.is_err());
    assert_eq!(pending.factories.load(Ordering::SeqCst), 1, "retirement must not authorize replacement native work");
    assert!(workspace.report_history("local-evidence").unwrap().is_empty());
}

#[tokio::test]
async fn simultaneous_native_failure_and_cancellation_preserves_the_native_error() {
    let cancellation = CollectionCancellation::default();
    let error = scan_until_cancelled(&cancellation, || async {
        cancellation.cancel();
        Err(CollectionStop::SourceChanged.error())
    })
    .await
    .unwrap_err();
    assert_eq!(CollectionStop::from_error(&error), CollectionStop::SourceChanged);
}

#[tokio::test]
async fn a_signal_delivery_error_does_not_replace_the_original_unconfirmed_reader_failure() {
    let cancellation = CollectionCancellation::default();
    let error = crate::collection_shutdown::with_signal(
        &cancellation,
        async {
            cancellation.cancelled().await;
            Err::<(), _>(CollectionStop::CancellationUnconfirmed.error())
        },
        async { Err(culprit_core::Error::internal("controlled signal delivery failure")) },
    )
    .await
    .unwrap_err();
    assert_eq!(CollectionStop::from_error(&error), CollectionStop::CancellationUnconfirmed);
}
