//! A case deadline signals cancellation, then awaits the same body. Only a borrowed query future may be
//! interrupted: its caller retains the native reader and must close it before returning.
use culprit_collector::capture::CollectionCancellation;
use culprit_core::{
    Result,
    telemetry::{CollectionStop, SourceShutdown},
};
use futures_util::{FutureExt, future::LocalBoxFuture};
use std::{future::Future, panic::AssertUnwindSafe, time::Duration};
use tokio::time::Instant;

/// Admission is checked before polling a command. Cancellation never drops a command which already started:
/// its original caller must await that result and retire the same reader/client before another phase.
pub(crate) fn require_active(cancellation: &CollectionCancellation) -> Result<()> {
    if cancellation.is_cancelled() { Err(CollectionStop::Cancelled.error()) } else { Ok(()) }
}

/// A completed failed phase withdraws peer admission. It publishes no submission/readiness signal and never
/// drops an already-started native future; its existing owner still has to settle and close before returning.
pub(crate) fn cancel_after_panic<T>(outcome: &std::thread::Result<T>, cancellation: &CollectionCancellation) {
    if outcome.is_err() {
        cancellation.cancel();
    }
}

pub(crate) async fn admitted<T>(
    cancellation: &CollectionCancellation,
    work: impl Future<Output = Result<T>>,
) -> Result<T> {
    require_active(cancellation)?;
    work.await
}
pub(crate) async fn batch(
    cancellation: &CollectionCancellation,
    client: &mut crate::conn::MsClient,
    sql: &str,
) -> Result<()> {
    admitted(cancellation, async {
        super::batch(client, sql).await;
        Ok(())
    })
    .await
}

/// This borrow adds admission only. All actual native work, counters and shutdown stay with the same reader.
pub(crate) struct AdmittedRead<'a, R> {
    reader: &'a mut R,
    cancellation: &'a CollectionCancellation,
}
impl<'a, R> AdmittedRead<'a, R> {
    pub fn new(reader: &'a mut R, cancellation: &'a CollectionCancellation) -> Self {
        Self { reader, cancellation }
    }
}
#[async_trait::async_trait]
impl<R: crate::read::NativeRead> crate::read::NativeRead for AdmittedRead<'_, R> {
    async fn query(
        &mut self,
        sql: &str,
        parameters: Vec<crate::read::Parameter>,
        limit: usize,
    ) -> crate::read::ReadResult<Vec<crate::value::Rec>> {
        require_active(self.cancellation).map_err(crate::read::ReadError::Source)?;
        self.reader.query(sql, parameters, limit).await
    }
    fn queries(&self) -> u32 {
        self.reader.queries()
    }
    fn connection_generation(&self) -> Option<u64> {
        self.reader.connection_generation()
    }
    fn read_metrics(&self) -> Option<culprit_core::telemetry::ReadWorkMetrics> {
        self.reader.read_metrics()
    }
    fn observe_session_work(&mut self, observation: crate::read::session_work::Observation) {
        self.reader.observe_session_work(observation);
    }
    async fn close(&mut self) -> SourceShutdown {
        self.reader.close().await
    }
}

const RETIREMENT_TIMEOUT: Duration = Duration::from_secs(5);
/// Physical retirement is bounded cleanup, never independent proof that an interrupted command settled.
pub(crate) async fn retire(client: crate::conn::MsClient) -> Result<()> {
    tokio::time::timeout(RETIREMENT_TIMEOUT, client.close())
        .await
        .map_err(|_| CollectionStop::CancellationUnconfirmed.error())?
        .map_err(crate::conn::map_error)
}

/// One locally scheduled fixture body returns its original owner even when the body panics. Scheduling is
/// separate from the deadline controller: the controller cancels and awaits this task, never aborts it.
pub(crate) async fn run_owned_body<O, T>(
    mut owner: O,
    body: impl for<'a> FnOnce(&'a mut O) -> LocalBoxFuture<'a, T>,
) -> (O, std::thread::Result<T>) {
    let outcome = AssertUnwindSafe(async {
        // Native work cannot be polled on the stack which submitted the task.
        tokio::task::yield_now().await;
        body(&mut owner).await
    })
    .catch_unwind()
    .await;
    (owner, outcome)
}

pub(crate) async fn finish_case<T>(
    deadline: Instant,
    cancellation: &CollectionCancellation,
    work: impl Future<Output = T>,
) -> (bool, T) {
    tokio::pin!(work);
    tokio::select! {
        value = &mut work => {
            let expired = Instant::now() >= deadline;
            if expired { cancellation.cancel(); }
            (expired, value)
        }
        _ = tokio::time::sleep_until(deadline) => {
            cancellation.cancel();
            (true, work.await)
        }
    }
}

pub(crate) async fn until_cancelled<T>(
    cancellation: &CollectionCancellation,
    work: impl Future<Output = Result<T>>,
) -> Result<T> {
    require_active(cancellation)?;
    tokio::pin!(work);
    tokio::select! {
        biased;
        _ = cancellation.cancelled() => Err(CollectionStop::Cancelled.error()),
        value = &mut work => value,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };

    #[tokio::test]
    async fn owned_body_is_polled_after_controller_yields_and_returns_the_same_owner() {
        use std::{cell::Cell, rc::Rc};
        let active_controller = Rc::new(Cell::new(false));
        let body_controller = active_controller.clone();
        let owner = Box::new(7u32);
        let address = owner.as_ref() as *const u32 as usize;
        let controller = async move {
            let task = tokio::task::spawn_local(run_owned_body(owner, move |owner| {
                Box::pin(async move {
                    assert!(!body_controller.get(), "the controller poll frame must have returned");
                    **owner = 11;
                    13
                })
            }));
            task.await.unwrap()
        };
        tokio::pin!(controller);
        let observed = std::future::poll_fn(|context| {
            active_controller.set(true);
            let value = controller.as_mut().poll(context);
            active_controller.set(false);
            value
        });
        let (owner, result) = tokio::task::LocalSet::new().run_until(observed).await;
        assert_eq!(owner.as_ref() as *const u32 as usize, address);
        assert_eq!(*owner, 11);
        assert_eq!(result.unwrap(), 13);
    }

    #[tokio::test]
    async fn cancelled_body_returns_its_original_owner_only_after_settlement() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let cancellation = CollectionCancellation::default();
                let body_cancellation = cancellation.clone();
                let (started, admitted) = tokio::sync::oneshot::channel();
                let (settling, settlement_started) = tokio::sync::oneshot::channel();
                let (release, settled) = tokio::sync::oneshot::channel();
                let returned = Arc::new(AtomicBool::new(false));
                let observed = returned.clone();
                let owner = Box::new(7u32);
                let address = owner.as_ref() as *const u32 as usize;
                let task = tokio::task::spawn_local(run_owned_body(owner, move |owner| {
                    Box::pin(async move {
                        started.send(()).unwrap();
                        body_cancellation.cancelled().await;
                        settling.send(()).unwrap();
                        settled.await.unwrap();
                        **owner = 11;
                        Err::<(), _>(CollectionStop::CancellationUnconfirmed.error())
                    })
                }));
                admitted.await.unwrap();
                let controller = async {
                    let outcome = finish_case(Instant::now(), &cancellation, task).await;
                    returned.store(true, Ordering::Release);
                    outcome
                };
                let settlement = async {
                    settlement_started.await.unwrap();
                    assert!(!observed.load(Ordering::Acquire), "cancellation must await the original body");
                    release.send(()).unwrap();
                };
                let ((expired, returned), ()) = tokio::join!(controller, settlement);
                let (owner, outcome) = returned.unwrap();
                assert!(expired && cancellation.is_cancelled());
                assert_eq!(owner.as_ref() as *const u32 as usize, address);
                assert_eq!(*owner, 11);
                assert_eq!(
                    CollectionStop::from_error(&outcome.unwrap().unwrap_err()),
                    CollectionStop::CancellationUnconfirmed
                );
            })
            .await;
    }

    #[tokio::test]
    async fn panicked_body_returns_the_original_owner_and_panic_before_controller_settlement() {
        let owner = Box::new(7u32);
        let address = owner.as_ref() as *const u32 as usize;
        let task = async move {
            tokio::task::spawn_local(run_owned_body::<_, ()>(owner, |owner| {
                Box::pin(async move {
                    **owner = 11;
                    std::panic::panic_any("original body panic");
                })
            }))
            .await
            .unwrap()
        };
        let (owner, outcome) = tokio::task::LocalSet::new().run_until(task).await;
        assert_eq!(owner.as_ref() as *const u32 as usize, address);
        assert_eq!(*owner, 11);
        assert_eq!(outcome.unwrap_err().downcast_ref::<&str>(), Some(&"original body panic"));
    }

    #[tokio::test]
    async fn original_failure_wakes_pending_peer_without_submission_or_followup_admission() {
        let cancellation = CollectionCancellation::default();
        cancel_after_panic(&Ok(()), &cancellation);
        assert!(!cancellation.is_cancelled(), "a successful original phase leaves the peer admitted");
        let submitting = tokio::sync::Notify::new();
        let followup_polled = AtomicBool::new(false);
        let peer = until_cancelled(&cancellation, async {
            submitting.notified().await;
            admitted(&cancellation, async {
                followup_polled.store(true, Ordering::Relaxed);
                Ok(())
            })
            .await
        });
        tokio::pin!(peer);
        std::future::poll_fn(|context| {
            assert!(peer.as_mut().poll(context).is_pending(), "peer really awaits the original submission");
            std::task::Poll::Ready(())
        })
        .await;
        let original: std::thread::Result<()> = Err(Box::new("original capture panic"));
        cancel_after_panic(&original, &cancellation);
        let error = peer.await.unwrap_err();
        assert_eq!(CollectionStop::from_error(&error), CollectionStop::Cancelled);
        assert!(!followup_polled.load(Ordering::Relaxed));
        assert_eq!(original.unwrap_err().downcast_ref::<&str>(), Some(&"original capture panic"));
        let submitted = submitting.notified();
        tokio::pin!(submitted);
        std::future::poll_fn(|context| {
            assert!(submitted.as_mut().poll(context).is_pending(), "no fake Submitted notification was sent");
            std::task::Poll::Ready(())
        })
        .await;
    }

    #[tokio::test]
    async fn case_deadline_awaits_original_owner_settlement_and_preserves_unconfirmed() {
        let cancellation = CollectionCancellation::default();
        let (settling, settlement_started) = tokio::sync::oneshot::channel();
        let (release, settlement_allowed) = tokio::sync::oneshot::channel();
        let finished = Arc::new(AtomicBool::new(false));
        let observed = finished.clone();
        let body = async {
            cancellation.cancelled().await;
            settling.send(()).unwrap();
            settlement_allowed.await.unwrap();
            SourceShutdown::Unconfirmed
        };
        let case = async {
            let result = finish_case(Instant::now(), &cancellation, body).await;
            finished.store(true, Ordering::Release);
            result
        };
        let controller = async {
            settlement_started.await.unwrap();
            assert!(!observed.load(Ordering::Acquire), "deadline cannot return while the original owner settles");
            release.send(()).unwrap();
        };
        let ((expired, shutdown), ()) = tokio::join!(case, controller);
        assert!(expired && cancellation.is_cancelled());
        assert_eq!(shutdown, SourceShutdown::Unconfirmed);
    }

    #[tokio::test]
    async fn ready_result_at_expired_deadline_still_signals_cancellation() {
        let cancellation = CollectionCancellation::default();
        let (expired, value) = finish_case(Instant::now(), &cancellation, async { 7 }).await;
        assert!(expired && cancellation.is_cancelled());
        assert_eq!(value, 7);
    }

    #[tokio::test]
    async fn cancelled_case_never_polls_a_followup_query() {
        let cancellation = CollectionCancellation::default();
        cancellation.cancel();
        let admitted = AtomicBool::new(false);
        let result = until_cancelled(&cancellation, async {
            admitted.store(true, Ordering::Release);
            Ok(())
        })
        .await;
        assert!(result.is_err());
        assert!(!admitted.load(Ordering::Acquire));
    }
    #[tokio::test]
    async fn admitted_work_never_starts_after_cancellation_but_started_work_keeps_its_original_result() {
        let cancellation = CollectionCancellation::default();
        let (started, observed) = tokio::sync::oneshot::channel();
        let (finish, finished) = tokio::sync::oneshot::channel();
        let work = admitted(&cancellation, async {
            started.send(()).unwrap();
            finished.await.unwrap();
            Err::<(), _>(CollectionStop::CancellationUnconfirmed.error())
        });
        let control = async {
            observed.await.unwrap();
            cancellation.cancel();
            finish.send(()).unwrap();
        };
        let (result, ()) = tokio::join!(work, control);
        assert_eq!(result.unwrap_err().detail, CollectionStop::CancellationUnconfirmed.error().detail);
        let calls = std::sync::atomic::AtomicUsize::new(0);
        let next = admitted(&cancellation, async {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
        .await;
        assert_eq!(next.unwrap_err().detail, CollectionStop::Cancelled.error().detail);
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn a_borrowed_read_stops_each_followup_dispatch_and_preserves_original_unconfirmed_close() {
        use crate::read::NativeRead;
        let cancellation = CollectionCancellation::default();
        let mut native = crate::resources::test_support::FakeRead::new(vec![Ok(vec![])]);
        native.shutdown = SourceShutdown::Unconfirmed;
        {
            let mut read = AdmittedRead::new(&mut native, &cancellation);
            assert!(read.query("first", vec![], 1).await.unwrap().is_empty());
            cancellation.cancel();
            assert_eq!(
                read.query("followup", vec![], 1).await.unwrap_err().into_error().detail,
                CollectionStop::Cancelled.error().detail
            );
            assert_eq!(read.queries(), 1);
            assert_eq!(read.close().await, SourceShutdown::Unconfirmed);
        }
        assert_eq!(native.calls.len(), 1);
        assert_eq!(native.closed, 1);
    }
}
