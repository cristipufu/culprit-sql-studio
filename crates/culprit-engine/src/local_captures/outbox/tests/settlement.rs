//! A queued directory read is woken only after the previous real filesystem owner has been released.
use super::*;
use culprit_collector::analytical::test_support::{
    MemoryClerkCase, hyperscale_log_component_evidence_cases, memory_clerk_evidence,
};
use std::{
    future::Future,
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    task::{Context, Poll, Wake, Waker},
};
use tokio::sync::Notify;

const BARRIER_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Exit {
    Success,
    ProjectionError,
    PostcheckCancellation,
    Panic,
    CallerDrop,
}

/// The waiting operation's waker parks permit release. The test can examine the actual directory lock at that
/// exact boundary, rather than race the next async resume or accept a mutex-only ownership check.
struct PermitRelease {
    arrived: Notify,
    release: Mutex<mpsc::Receiver<()>>,
    seen: AtomicBool,
}
impl Wake for PermitRelease {
    fn wake(self: Arc<Self>) {
        if !self.seen.swap(true, Ordering::SeqCst) {
            self.arrived.notify_one();
            // Never panic from a waker invoked during another worker's unwind.
            if let Ok(release) = self.release.lock() {
                let _ = release.recv_timeout(BARRIER_TIMEOUT);
            }
        }
    }
}

async fn fixture(analysis: LocalCaptureAnalysisKind) -> LocalCaptureFixture {
    match analysis {
        LocalCaptureAnalysisKind::PageServerReads => {
            let case = page_server_read_evidence_cases().remove(0);
            LocalCaptureFixture::query_history(case.source, case.records).await
        }
        LocalCaptureAnalysisKind::HyperscaleLogComponents => {
            let (source, rows) = hyperscale_log_component_evidence_cases().remove(0);
            LocalCaptureFixture::snapshot(source, rows).await
        }
        LocalCaptureAnalysisKind::MemoryClerks => {
            let (source, rows) = memory_clerk_evidence(MemoryClerkCase::SharedInstance);
            LocalCaptureFixture::snapshot(source, rows).await
        }
        other => {
            let (source, rows) = super::snapshots::originals(other);
            LocalCaptureFixture::snapshot(source, rows).await
        }
    }
}

async fn assert_settlement(analysis: LocalCaptureAnalysisKind, exit: Exit) {
    let mut fixture = fixture(analysis).await;
    drop(fixture.outbox.take());
    let projection_return = Arc::new(Notify::new());
    let (release_projection, projection_released) = mpsc::sync_channel(1);
    let projection_released = Mutex::new(projection_released);
    let used = AtomicBool::new(false);
    let arrived = projection_return.clone();
    let mut reader =
        OutboxCaptureReader::directory(fixture.directory.clone(), CompletedCaptureBudget::default()).unwrap();
    reader.projection_boundary = Some(Arc::new(move || {
        if used.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        arrived.notify_one();
        projection_released
            .lock()
            .map_err(|_| DeliveryProblem::Storage.error())?
            .recv_timeout(BARRIER_TIMEOUT)
            .map_err(|_| DeliveryProblem::Storage.error())?;
        match exit {
            Exit::ProjectionError => Err(DeliveryProblem::Conflict.error()),
            Exit::Panic => panic!("controlled original projection panic"),
            _ => Ok(()),
        }
    }));
    let reader = Arc::new(reader);
    let mut guard = Some(OperationGuard::new(reader.timeout()).unwrap());
    let operation = guard.as_ref().unwrap().handle();
    let first_reader = reader.clone();
    let id = fixture.manifest.capture.observation_id.clone();
    let mut first = Some(tokio::spawn(async move {
        first_reader
            .analyze(LocalCaptureRequest { connection: "local-evidence", observation_id: &id, analysis, operation })
            .await
    }));
    tokio::time::timeout(BARRIER_TIMEOUT, projection_return.notified()).await.unwrap();
    assert_eq!(reader.readers.available_permits(), 0);

    let (release_waiter, waiter_released) = mpsc::sync_channel(1);
    let release = Arc::new(PermitRelease {
        arrived: Notify::new(),
        release: Mutex::new(waiter_released),
        seen: AtomicBool::new(false),
    });
    let waker = Waker::from(release.clone());
    let second_guard = OperationGuard::new(reader.timeout()).unwrap();
    let mut queued = reader.analyze(LocalCaptureRequest {
        connection: "local-evidence",
        observation_id: &fixture.manifest.capture.observation_id,
        analysis,
        operation: second_guard.handle(),
    });
    assert!(matches!(Future::poll(queued.as_mut(), &mut Context::from_waker(&waker)), Poll::Pending));
    match exit {
        Exit::PostcheckCancellation => drop(guard.take()),
        Exit::CallerDrop => {
            first.as_ref().unwrap().abort();
            drop(guard.take());
            assert!(first.take().unwrap().await.unwrap_err().is_cancelled());
        }
        _ => {}
    }
    assert_eq!(reader.readers.available_permits(), 0, "the unsettled callback still retains the gate");
    release_projection.send(()).unwrap();
    tokio::time::timeout(BARRIER_TIMEOUT, release.arrived.notified()).await.unwrap();

    // This is the same exclusive directory acquisition the queued production reader performs next. The old
    // closure-local permit woke it while the preceding worker/async Outbox still held this exact owner.lock.
    let reopened = Outbox::open(&fixture.directory);
    let unlocked = reopened.is_ok();
    drop(reopened);
    let _ = release_waiter.send(());
    assert!(unlocked, "{analysis:?}/{exit:?}: a queued directory action was woken before filesystem ownership ended");

    if let Some(first) = first {
        let result = first.await;
        match exit {
            Exit::Success => assert_eq!(result.unwrap().unwrap().kind(), analysis),
            Exit::ProjectionError => {
                assert_eq!(result.unwrap().unwrap_err().detail, Some(ErrorDetail::Delivery(DeliveryProblem::Conflict)));
            }
            Exit::PostcheckCancellation => {
                assert_eq!(
                    result.unwrap().unwrap_err().detail,
                    Some(ErrorDetail::CollectionStopped(CollectionStop::Cancelled))
                );
            }
            Exit::Panic => {
                assert_eq!(result.unwrap().unwrap_err().detail, Some(ErrorDetail::Outbox(OutboxProblem::Storage)));
            }
            Exit::CallerDrop => unreachable!("caller drop was joined while the worker still held the barrier"),
        }
    }
    assert_eq!(queued.await.unwrap().kind(), analysis, "the waiting action uses the same reader without Locked");
    assert_eq!(reader.readers.available_permits(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn directory_gate_spans_projection_postcheck_worker_and_async_owner_settlement_for_every_analysis() {
    for analysis in LocalCaptureAnalysisKind::ALL {
        for exit in [Exit::Success, Exit::ProjectionError, Exit::PostcheckCancellation, Exit::Panic, Exit::CallerDrop] {
            assert_settlement(analysis, exit).await;
        }
    }
}
