//! Termination requests cancellation, then awaits the existing controller's native shutdown proof.
use culprit_collector::capture::CollectionCancellation;
use culprit_core::Result;
use std::future::Future;

pub(crate) async fn run<T>(cancellation: &CollectionCancellation, work: impl Future<Output = Result<T>>) -> Result<T> {
    #[cfg(unix)]
    let mut terminate =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).map_err(|_| signal_failed())?;
    let signal = async {
        #[cfg(unix)]
        {
            tokio::select! {
                result=tokio::signal::ctrl_c()=>result.map_err(|_|signal_failed()),
                result=terminate.recv()=>result.ok_or_else(signal_failed),
            }
        }
        #[cfg(not(unix))]
        tokio::signal::ctrl_c().await.map_err(|_| signal_failed())
    };
    with_signal(cancellation, work, signal).await
}
async fn with_signal<T>(
    cancellation: &CollectionCancellation,
    work: impl Future<Output = Result<T>>,
    signal: impl Future<Output = Result<()>>,
) -> Result<T> {
    tokio::pin!(work);
    tokio::select! {
        result=&mut work=>result,
        signal=signal=>{
            cancellation.cancel();
            let result=work.await;
            signal?;
            result
        }
    }
}
fn signal_failed() -> culprit_core::Error {
    culprit_core::Error::internal(culprit_core::t!("collection.signal_failed"))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn shutdown_awaits_the_controller_and_keeps_an_unconfirmed_shutdown_error() {
        let cancellation = CollectionCancellation::default();
        let result = with_signal(
            &cancellation,
            async {
                cancellation.cancelled().await;
                Err::<(), _>(culprit_core::telemetry::CollectionStop::CancellationUnconfirmed.error())
            },
            async { Ok(()) },
        )
        .await;
        assert!(cancellation.is_cancelled());
        assert_eq!(
            result.unwrap_err().detail,
            Some(culprit_core::ErrorDetail::CollectionStopped(
                culprit_core::telemetry::CollectionStop::CancellationUnconfirmed
            ))
        );
    }
}
