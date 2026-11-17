//! A fixture deadline cancels native admission, then joins the source owner instead of dropping its future.
use culprit_collector::capture::CollectionCancellation;
use std::{future::Future, time::Duration};

pub(super) struct Completion<T> {
    pub value: T,
    pub deadline_reached: bool,
}

pub(super) async fn finish<T>(
    timeout: Duration,
    cancellation: &CollectionCancellation,
    body: impl Future<Output = T>,
) -> Completion<T> {
    let deadline = tokio::time::Instant::now() + timeout;
    tokio::pin!(body);
    tokio::select! {
        biased;
        value=&mut body=>{
            let deadline_reached=tokio::time::Instant::now()>=deadline;
            if deadline_reached { cancellation.cancel(); }
            Completion{value,deadline_reached}
        },
        _=tokio::time::sleep_until(deadline)=>{
            cancellation.cancel();
            Completion{value:body.await,deadline_reached:true}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use culprit_core::telemetry::CollectionStop;
    use tokio::sync::oneshot;

    #[tokio::test]
    async fn a_case_deadline_waits_for_owned_shutdown_and_preserves_unconfirmed_result() {
        let cancellation = CollectionCancellation::default();
        let native = cancellation.clone();
        let (started, waiting) = oneshot::channel();
        let (release, shutdown) = oneshot::channel();
        let task = tokio::spawn(async move {
            finish(Duration::ZERO, &cancellation, async move {
                native.cancelled().await;
                started.send(()).unwrap();
                shutdown.await.unwrap();
                Err::<(), _>(CollectionStop::CancellationUnconfirmed.error())
            })
            .await
        });
        waiting.await.unwrap();
        assert!(!task.is_finished(), "elapsed case time cannot release an unsettled native owner");
        release.send(()).unwrap();
        let completed = task.await.unwrap();
        assert!(completed.deadline_reached);
        assert_eq!(CollectionStop::from_error(&completed.value.unwrap_err()), CollectionStop::CancellationUnconfirmed);
    }

    #[tokio::test]
    async fn a_completed_case_does_not_cancel_later_owned_cleanup() {
        let cancellation = CollectionCancellation::default();
        let completed = finish(Duration::from_secs(10), &cancellation, async { 42 }).await;
        assert_eq!(completed.value, 42);
        assert!(!completed.deadline_reached);
        assert!(!cancellation.is_cancelled());
    }
    #[tokio::test]
    async fn a_ready_body_cannot_clear_an_already_reached_case_deadline() {
        let cancellation = CollectionCancellation::default();
        let completed = finish(Duration::ZERO, &cancellation, async { 42 }).await;
        assert_eq!(completed.value, 42, "the completed owner's result is retained");
        assert!(completed.deadline_reached, "body-ready and deadline-ready must not accept an elapsed case");
        assert!(cancellation.is_cancelled());
    }
}
