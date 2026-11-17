//! A case deadline signals cancellation, then awaits the same body. Only a borrowed query future may be
//! interrupted: its caller retains the native reader and must close it before returning.
use culprit_collector::capture::CollectionCancellation;
use culprit_core::{
    Result,
    telemetry::{CollectionStop, SourceShutdown},
};
use std::future::Future;
use tokio::time::Instant;

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
    if cancellation.is_cancelled() {
        return Err(CollectionStop::Cancelled.error());
    }
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
}
