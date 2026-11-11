//! The host composes source and delivery lifetimes; neither loop owns or cancels the other's I/O future.
use crate::lifecycle::with_background;
use crate::{capture::CollectionCancellation, delivery::worker::DeliveryWorker, fleet::FleetRunner};
use culprit_core::{Result, telemetry::*};
use std::future::Future;

pub trait ServiceReporter: Sync {
    fn collection(&self, event: &FleetCollectionEvent) -> Result<()>;
    fn delivery(&self, previous: Option<&DeliveryEntry>, result: Result<DeliveryEntry>) -> Result<()>;
    fn source_paused(&self, error: &culprit_core::Error) -> Result<()>;
}

pub struct CollectorService<'a> {
    source: FleetRunner<'a>,
    delivery: DeliveryWorker<'a>,
}
impl<'a> CollectorService<'a> {
    pub fn new(source: FleetRunner<'a>, delivery: DeliveryWorker<'a>) -> Self {
        Self { source, delivery }
    }
    /// Stop admitting new work, settle the current native operation and the current delivery transition,
    /// then return. Undelivered committed work remains durable; shutdown does not wait for a healthy target.
    pub async fn run(
        &mut self,
        cancellation: &CollectionCancellation,
        max_work: Option<u32>,
        reporter: &dyn ServiceReporter,
    ) -> Result<FleetSummary> {
        let source = source_lifetime(
            cancellation,
            reporter,
            max_work,
            self.source.run(cancellation, max_work, |event| reporter.collection(event)),
        );
        let delivery = run_delivery(&mut self.delivery, cancellation, reporter);
        with_background(cancellation, source, delivery).await
    }
}

pub async fn run_delivery(
    worker: &mut DeliveryWorker<'_>,
    cancellation: &CollectionCancellation,
    reporter: &dyn ServiceReporter,
) -> Result<()> {
    worker.run(cancellation, &|id, result| reporter.delivery(id, result)).await
}

async fn source_lifetime<T>(
    cancellation: &CollectionCancellation,
    reporter: &dyn ServiceReporter,
    work_limit: Option<u32>,
    work: impl Future<Output = Result<T>>,
) -> Result<T> {
    let result = work.await;
    if let Err(error) = &result
        && matches!(
            error.detail,
            Some(
                culprit_core::ErrorDetail::CollectionStopped(CollectionStop::CancellationUnconfirmed)
                    | culprit_core::ErrorDetail::Outbox(OutboxProblem::SourceUnconfirmed)
            )
        )
    {
        // Source ownership is fenced permanently for this process. Keep archive delivery and probes alive;
        // acknowledging native quiescence requires an operator and a new source owner, never an automatic retry.
        reporter.source_paused(error)?;
        if work_limit.is_none() {
            cancellation.cancelled().await;
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    struct Paused(tokio::sync::Notify);
    impl ServiceReporter for Paused {
        fn collection(&self, _: &FleetCollectionEvent) -> Result<()> {
            unreachable!()
        }
        fn delivery(&self, _: Option<&DeliveryEntry>, _: Result<DeliveryEntry>) -> Result<()> {
            unreachable!()
        }
        fn source_paused(&self, _: &culprit_core::Error) -> Result<()> {
            self.0.notify_one();
            Ok(())
        }
    }
    #[tokio::test]
    async fn a_fenced_source_stops_native_admission_but_does_not_stop_existing_delivery_or_lose_its_error_on_shutdown()
    {
        let cancellation = CollectionCancellation::default();
        let paused = Paused(tokio::sync::Notify::new());
        let delivered = AtomicBool::new(false);
        let source = source_lifetime(&cancellation, &paused, None, async {
            Err::<(), _>(CollectionStop::CancellationUnconfirmed.error())
        });
        let delivery = async {
            paused.0.notified().await;
            assert!(!cancellation.is_cancelled());
            delivered.store(true, Ordering::Release);
            cancellation.cancel();
            Ok(())
        };
        let result = with_background(&cancellation, source, delivery).await;
        assert!(delivered.load(Ordering::Acquire));
        assert_eq!(
            result.unwrap_err().detail,
            Some(culprit_core::ErrorDetail::CollectionStopped(CollectionStop::CancellationUnconfirmed))
        );
        let result = source_lifetime(&CollectionCancellation::default(), &paused, Some(1), async {
            Err::<(), _>(CollectionStop::CancellationUnconfirmed.error())
        })
        .await;
        assert!(result.is_err(), "explicit finite work must return its fence rather than becoming a daemon");
    }

    #[tokio::test]
    async fn fatal_delivery_waits_for_native_shutdown_and_preserves_unconfirmed_source_ownership() {
        let cancellation = CollectionCancellation::default();
        let stopped = AtomicBool::new(false);
        let source = async {
            cancellation.cancelled().await;
            tokio::task::yield_now().await;
            stopped.store(true, Ordering::Release);
            Err::<(), _>(CollectionStop::CancellationUnconfirmed.error())
        };
        let result = with_background(&cancellation, source, async { Err(DeliveryProblem::Fenced.error()) }).await;
        assert!(stopped.load(Ordering::Acquire));
        assert_eq!(
            result.unwrap_err().detail,
            Some(culprit_core::ErrorDetail::CollectionStopped(CollectionStop::CancellationUnconfirmed))
        );
    }

    #[tokio::test]
    async fn completed_source_work_awaits_the_admitted_delivery_transition() {
        let cancellation = CollectionCancellation::default();
        let saved = AtomicBool::new(false);
        let delivery = async {
            cancellation.cancelled().await;
            tokio::task::yield_now().await;
            saved.store(true, Ordering::Release);
            Ok(())
        };
        assert_eq!(with_background(&cancellation, async { Ok(7) }, delivery).await.unwrap(), 7);
        assert!(saved.load(Ordering::Acquire));
    }
}
