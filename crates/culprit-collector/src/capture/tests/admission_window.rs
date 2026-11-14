use super::*;
use std::sync::{
    Mutex,
    atomic::{AtomicBool, AtomicU32, Ordering},
};
use tokio::sync::Notify;

#[derive(Default)]
struct Probe {
    opened: Notify,
    observed: Notify,
    closing: Notify,
    open_calls: AtomicU32,
    created_sources: AtomicU32,
    native_reads: AtomicU32,
    close_calls: AtomicU32,
    active_read: AtomicBool,
    close_started_at: Mutex<Option<Instant>>,
    close_finished: AtomicBool,
}
#[derive(Clone, Copy)]
enum Close {
    QuietAfter(Duration),
    NeverConfirms,
}
#[derive(Clone)]
struct AdmittedFactory {
    window: CollectionAdmissionWindow,
    authentication_delay: Duration,
    close: Close,
    probe: Arc<Probe>,
}
struct PendingSource {
    close: Close,
    probe: Arc<Probe>,
}
struct ActiveRead(Arc<Probe>);
impl Drop for ActiveRead {
    fn drop(&mut self) {
        self.0.active_read.store(false, Ordering::SeqCst);
    }
}
#[async_trait::async_trait]
impl SourceFactory for AdmittedFactory {
    fn admission_window(&self) -> Option<CollectionAdmissionWindow> {
        Some(self.window)
    }
    async fn open(&self, _: &CollectionRequest) -> Result<Box<dyn TelemetrySource>> {
        self.probe.open_calls.fetch_add(1, Ordering::SeqCst);
        self.probe.opened.notify_one();
        if !self.authentication_delay.is_zero() {
            tokio::time::sleep(self.authentication_delay).await;
        }
        self.probe.created_sources.fetch_add(1, Ordering::SeqCst);
        Ok(Box::new(PendingSource { close: self.close, probe: self.probe.clone() }))
    }
}
#[async_trait::async_trait]
impl TelemetrySource for PendingSource {
    async fn describe(&mut self) -> Result<CollectionDescription> {
        self.probe.native_reads.fetch_add(1, Ordering::SeqCst);
        self.probe.active_read.store(true, Ordering::SeqCst);
        let _active = ActiveRead(self.probe.clone());
        self.probe.observed.notify_one();
        std::future::pending().await
    }
    fn query_count(&self) -> u32 {
        self.probe.native_reads.load(Ordering::SeqCst)
    }
    async fn read_page(&mut self, _: TelemetryStream, _: Option<&str>) -> Result<CollectionRead> {
        panic!("the pending description cannot admit a page read")
    }
    async fn finish(&mut self) -> Result<SourceCompletion> {
        panic!("the interrupted description cannot verify completion")
    }
    async fn close(&mut self) -> SourceShutdown {
        assert!(
            !self.probe.active_read.load(Ordering::SeqCst),
            "the native read future must be dropped before close owns shutdown"
        );
        self.probe.close_calls.fetch_add(1, Ordering::SeqCst);
        *self.probe.close_started_at.lock().unwrap() = Some(Instant::now());
        self.probe.closing.notify_one();
        match self.close {
            Close::QuietAfter(delay) => {
                if !delay.is_zero() {
                    tokio::time::sleep(delay).await;
                }
                self.probe.close_finished.store(true, Ordering::SeqCst);
                SourceShutdown::Quiescent
            }
            Close::NeverConfirms => std::future::pending().await,
        }
    }
}

fn request() -> CollectionRequest {
    CollectionRequest {
        subject: CollectionSubject::Database,
        selection: CollectionSelection::Snapshot { dataset: SnapshotDataset::Operational },
        budget: CollectionBudget { statement_timeout_ms: 1_000, total_timeout_ms: 10_000, ..Default::default() },
    }
}
fn factory(request: &CollectionRequest, authentication_delay: Duration, close: Close) -> AdmittedFactory {
    AdmittedFactory {
        window: CollectionAdmissionWindow::new(Instant::now(), &request.budget).unwrap(),
        authentication_delay,
        close,
        probe: Arc::new(Probe::default()),
    }
}
async fn prepared(path: &Path, request: &CollectionRequest) -> ObservationWriter {
    ObservationWriter::create(path.into(), request.clone(), culprit_read::admission::observation_identity().unwrap())
        .await
        .unwrap()
}
fn start_prepared(
    gate: Arc<SourceGate>,
    factory: AdmittedFactory,
    request: CollectionRequest,
    files: ObservationWriter,
    cancellation: CollectionCancellation,
) -> tokio::task::JoinHandle<Result<CollectionManifest>> {
    tokio::spawn(async move {
        let window = factory.admission_window();
        collect_prepared(&gate, &factory, request, files, &cancellation, None, window).await
    })
}
fn assert_no_native(probe: &Probe) {
    assert_eq!(probe.native_reads.load(Ordering::SeqCst), 0);
    assert!(!probe.active_read.load(Ordering::SeqCst));
}

/// Holding the real blocking pool delays filesystem/journal work, without changing production I/O or using long sleeps.
struct BlockedStorage {
    release: Option<std::sync::mpsc::Sender<()>>,
    task: tokio::task::JoinHandle<()>,
}
impl BlockedStorage {
    async fn hold() -> Self {
        let (release, waiting) = std::sync::mpsc::channel();
        let (entered, ready) = tokio::sync::oneshot::channel();
        let task = tokio::task::spawn_blocking(move || {
            let _ = entered.send(());
            let _ = waiting.recv();
        });
        ready.await.unwrap();
        Self { release: Some(release), task }
    }
    async fn finish(mut self) {
        self.release.take().unwrap().send(()).unwrap();
        (&mut self.task).await.unwrap();
    }
}
impl Drop for BlockedStorage {
    fn drop(&mut self) {
        if let Some(release) = self.release.take() {
            let _ = release.send(());
        }
    }
}
fn storage_runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .start_paused(true)
        .max_blocking_threads(1)
        .build()
        .unwrap()
}

#[test]
fn artifact_creation_cannot_refresh_an_expired_native_start_reservation() {
    storage_runtime().block_on(async {
        let directory = tempfile::tempdir().unwrap();
        let request = request();
        let factory = factory(&request, Duration::ZERO, Close::QuietAfter(Duration::ZERO));
        let probe = factory.probe.clone();
        let gate = Arc::new(SourceGate::default());
        let storage = BlockedStorage::hold().await;
        let path = directory.path().join("capture");
        let capture_gate = gate.clone();
        let capture = tokio::spawn(async move {
            collect_once(&capture_gate, &factory, request, &path, &CollectionCancellation::default()).await
        });
        tokio::task::yield_now().await;
        assert!(!capture.is_finished(), "the artifact is waiting for its real filesystem owner");
        tokio::time::advance(SOURCE_ADMISSION_START_WINDOW + Duration::from_millis(1)).await;
        storage.finish().await;
        let manifest = capture.await.unwrap().unwrap();
        assert_eq!(manifest.status, CollectionStatus::Incomplete(CollectionStop::TimedOut));
        assert_eq!(manifest.source_queries, 0);
        assert_eq!(probe.open_calls.load(Ordering::SeqCst), 0);
        assert_no_native(&probe);
        assert!(!gate.is_closed());
        drop(gate.acquire().await.unwrap());
    });
}

#[test]
fn late_source_lease_write_withholds_observe_and_closes_the_created_source_once() {
    storage_runtime().block_on(async {
        let directory = tempfile::tempdir().unwrap();
        let request = request();
        let mut outbox =
            crate::outbox::Outbox::create(&directory.path().join("outbox"), OutboxLimits::default()).unwrap();
        let mut lease = outbox.reserve("fixture", &request).await.unwrap();
        let files = ObservationWriter::create(lease.directory().into(), request.clone(), lease.identity().clone())
            .await
            .unwrap();
        let factory = factory(&request, Duration::ZERO, Close::QuietAfter(Duration::ZERO));
        let probe = factory.probe.clone();
        let gate = SourceGate::default();
        let cancellation = CollectionCancellation::default();
        let storage = BlockedStorage::hold().await;
        let manifest = {
            let capture = collect_prepared(
                &gate,
                &factory,
                request,
                files,
                &cancellation,
                Some(&mut lease),
                factory.admission_window(),
            );
            tokio::pin!(capture);
            tokio::select! {
                biased;
                result = &mut capture => panic!("lease storage should block capture: {result:?}"),
                _ = probe.opened.notified() => {},
            }
            assert_eq!(probe.created_sources.load(Ordering::SeqCst), 1);
            tokio::time::advance(SOURCE_ADMISSION_START_WINDOW + Duration::from_millis(1)).await;
            storage.finish().await;
            capture.await.unwrap()
        };
        assert_eq!(manifest.status, CollectionStatus::Incomplete(CollectionStop::TimedOut));
        assert_eq!(probe.close_calls.load(Ordering::SeqCst), 1);
        assert!(probe.close_finished.load(Ordering::SeqCst));
        assert_no_native(&probe);
        assert!(!gate.is_closed());
        lease.finish().await.unwrap();
        let status = outbox.status(10, None).await.unwrap();
        assert_eq!(status.entries.len(), 1);
        assert_eq!(status.entries[0].source_work, SourceWorkState::Quiescent);
    });
}

#[tokio::test(start_paused = true)]
async fn a_busy_native_permit_cannot_admit_factory_or_observe_after_start_expiry() {
    let directory = tempfile::tempdir().unwrap();
    let request = request();
    let files = prepared(&directory.path().join("capture"), &request).await;
    let factory = factory(&request, Duration::ZERO, Close::QuietAfter(Duration::ZERO));
    let probe = factory.probe.clone();
    let gate = Arc::new(SourceGate::default());
    let prior = gate.acquire().await.unwrap();
    let capture = start_prepared(gate.clone(), factory, request, files, CollectionCancellation::default());
    tokio::task::yield_now().await;
    tokio::time::advance(SOURCE_ADMISSION_START_WINDOW + Duration::from_millis(1)).await;
    let manifest = capture.await.unwrap().unwrap();
    assert_eq!(manifest.status, CollectionStatus::Incomplete(CollectionStop::TimedOut));
    assert_eq!(probe.open_calls.load(Ordering::SeqCst), 0);
    assert_no_native(&probe);
    drop(prior);
    assert!(!gate.is_closed());
    drop(gate.acquire().await.unwrap());
}

#[tokio::test(start_paused = true)]
async fn slow_authentication_is_canceled_at_the_original_start_deadline_without_creating_a_source() {
    let directory = tempfile::tempdir().unwrap();
    let request = request();
    let files = prepared(&directory.path().join("capture"), &request).await;
    let factory = factory(&request, Duration::from_secs(30), Close::QuietAfter(Duration::ZERO));
    let probe = factory.probe.clone();
    let gate = Arc::new(SourceGate::default());
    let capture = start_prepared(gate.clone(), factory, request, files, CollectionCancellation::default());
    probe.opened.notified().await;
    tokio::time::advance(SOURCE_ADMISSION_START_WINDOW + Duration::from_millis(1)).await;
    let manifest = capture.await.unwrap().unwrap();
    assert_eq!(manifest.status, CollectionStatus::Incomplete(CollectionStop::TimedOut));
    tokio::time::advance(Duration::from_secs(30)).await;
    assert_eq!(probe.open_calls.load(Ordering::SeqCst), 1);
    assert_eq!(probe.created_sources.load(Ordering::SeqCst), 0);
    assert_eq!(probe.close_calls.load(Ordering::SeqCst), 0);
    assert_no_native(&probe);
    assert!(!gate.is_closed());
}

#[tokio::test(start_paused = true)]
async fn authentication_and_a_wider_later_request_never_extend_the_saved_absolute_finish() {
    let directory = tempfile::tempdir().unwrap();
    let request = request();
    let files = prepared(&directory.path().join("capture"), &request).await;
    let mut saved_budget = request.budget.clone();
    saved_budget.total_timeout_ms = 2_000;
    let admitted_at = Instant::now();
    let window = CollectionAdmissionWindow::new(admitted_at, &saved_budget).unwrap();
    assert_eq!(window.finish_before(), admitted_at + SOURCE_ADMISSION_START_WINDOW + Duration::from_secs(2));
    let mut factory = factory(&request, Duration::from_secs(4), Close::QuietAfter(Duration::ZERO));
    factory.window = window;
    let probe = factory.probe.clone();
    let gate = Arc::new(SourceGate::default());
    let capture = start_prepared(gate.clone(), factory, request, files, CollectionCancellation::default());
    probe.opened.notified().await;
    tokio::time::advance(Duration::from_secs(4)).await;
    probe.observed.notified().await;
    assert!(probe.active_read.load(Ordering::SeqCst));
    tokio::time::advance(Duration::from_secs(3)).await;
    let manifest = capture.await.unwrap().unwrap();
    assert_eq!(manifest.status, CollectionStatus::Incomplete(CollectionStop::TimedOut));
    assert_eq!(*probe.close_started_at.lock().unwrap(), Some(window.finish_before()));
    assert!(probe.close_finished.load(Ordering::SeqCst));
    assert_eq!(probe.native_reads.load(Ordering::SeqCst), 1);
    assert_eq!(probe.close_calls.load(Ordering::SeqCst), 1);
    assert!(!probe.active_read.load(Ordering::SeqCst));
    assert!(!gate.is_closed());
}

#[tokio::test(start_paused = true)]
async fn cancellation_during_authentication_releases_unused_admission_without_late_source_work() {
    let directory = tempfile::tempdir().unwrap();
    let request = request();
    let files = prepared(&directory.path().join("capture"), &request).await;
    let factory = factory(&request, Duration::from_secs(30), Close::QuietAfter(Duration::ZERO));
    let probe = factory.probe.clone();
    let gate = Arc::new(SourceGate::default());
    let cancellation = CollectionCancellation::default();
    let capture = start_prepared(gate.clone(), factory, request, files, cancellation.clone());
    probe.opened.notified().await;
    cancellation.cancel();
    let manifest = capture.await.unwrap().unwrap();
    assert_eq!(manifest.status, CollectionStatus::Incomplete(CollectionStop::Cancelled));
    tokio::time::advance(Duration::from_secs(30)).await;
    assert_no_native(&probe);
    assert_eq!(probe.created_sources.load(Ordering::SeqCst), 0);
    assert!(!gate.is_closed());
    drop(gate.acquire().await.unwrap());
}

#[tokio::test(start_paused = true)]
async fn cancellation_drops_the_active_read_and_holds_admission_until_confirmed_close() {
    let directory = tempfile::tempdir().unwrap();
    let request = request();
    let files = prepared(&directory.path().join("capture"), &request).await;
    let factory = factory(&request, Duration::ZERO, Close::QuietAfter(Duration::from_secs(1)));
    let probe = factory.probe.clone();
    let gate = Arc::new(SourceGate::default());
    let cancellation = CollectionCancellation::default();
    let capture = start_prepared(gate.clone(), factory, request, files, cancellation.clone());
    probe.observed.notified().await;
    cancellation.cancel();
    probe.closing.notified().await;
    assert!(!probe.active_read.load(Ordering::SeqCst));
    assert!(!probe.close_finished.load(Ordering::SeqCst));
    assert!(!capture.is_finished());
    let ready = gate.acquire();
    tokio::pin!(ready);
    tokio::select! { biased; _ = &mut ready => panic!("close still owns native admission"), _ = tokio::task::yield_now() => {} }
    tokio::time::advance(Duration::from_secs(1)).await;
    let manifest = capture.await.unwrap().unwrap();
    assert_eq!(manifest.status, CollectionStatus::Incomplete(CollectionStop::Cancelled));
    assert_eq!(probe.native_reads.load(Ordering::SeqCst), 1);
    assert_eq!(probe.close_calls.load(Ordering::SeqCst), 1);
    assert!(probe.close_finished.load(Ordering::SeqCst));
    drop(ready.await.unwrap());
    assert!(!gate.is_closed());
}

#[tokio::test(start_paused = true)]
async fn unconfirmed_close_fences_following_native_work_after_the_bounded_shutdown_window() {
    let directory = tempfile::tempdir().unwrap();
    let request = request();
    let files = prepared(&directory.path().join("capture"), &request).await;
    let factory = factory(&request, Duration::ZERO, Close::NeverConfirms);
    let probe = factory.probe.clone();
    let gate = Arc::new(SourceGate::default());
    let cancellation = CollectionCancellation::default();
    let capture = start_prepared(gate.clone(), factory, request, files, cancellation.clone());
    probe.observed.notified().await;
    cancellation.cancel();
    probe.closing.notified().await;
    tokio::time::advance(SOURCE_SHUTDOWN_TIMEOUT).await;
    let manifest = capture.await.unwrap().unwrap();
    assert_eq!(manifest.status, CollectionStatus::Incomplete(CollectionStop::CancellationUnconfirmed));
    assert_eq!(probe.close_calls.load(Ordering::SeqCst), 1);
    assert!(!probe.close_finished.load(Ordering::SeqCst));
    assert!(!probe.active_read.load(Ordering::SeqCst));
    assert!(gate.is_closed());
    assert!(gate.acquire().await.is_err());
}
