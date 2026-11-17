//! A stateful blocking operation keeps its ownership until its worker ends, even when its caller is retired.
use culprit_core::{Error, Result};
use std::sync::Arc;
use tokio::sync::{Mutex, OwnedMutexGuard};
pub(crate) struct BlockingState<S> {
    state: Arc<Mutex<S>>,
    worker_failure: fn() -> Error,
}
impl<S> Clone for BlockingState<S> {
    fn clone(&self) -> Self {
        Self { state: self.state.clone(), worker_failure: self.worker_failure }
    }
}
impl<S: Send + 'static> BlockingState<S> {
    pub fn new(state: S, worker_failure: fn() -> Error) -> Self {
        Self { state: Arc::new(Mutex::new(state)), worker_failure }
    }
    pub async fn acquire(&self) -> BlockingAccess<S> {
        BlockingAccess { state: self.state.clone().lock_owned().await, worker_failure: self.worker_failure }
    }
    pub async fn run<P, F, T>(&self, prepare: P) -> Result<T>
    where
        P: FnOnce() -> F + Send,
        F: FnOnce(&mut S) -> Result<T> + Send + 'static,
        T: Send + 'static,
    {
        let access = self.acquire().await;
        access.run(prepare()).await
    }
}

pub(crate) struct BlockingAccess<S> {
    state: OwnedMutexGuard<S>,
    worker_failure: fn() -> Error,
}
impl<S: Send + 'static> BlockingAccess<S> {
    pub async fn run<F, T>(mut self, operation: F) -> Result<T>
    where
        F: FnOnce(&mut S) -> Result<T> + Send + 'static,
        T: Send + 'static,
    {
        let worker_failure = self.worker_failure;
        let language = culprit_core::i18n::current();
        tokio::task::spawn_blocking(move || culprit_core::i18n::scope(&language.code, || operation(&mut self.state)))
            .await
            .map_err(|_| worker_failure())?
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn worker_results_and_errors_inherit_the_scoped_callers_language() {
        use culprit_core::{ErrorDetail, i18n, telemetry::DeliveryProblem};
        let state = BlockingState::new((), || DeliveryProblem::Storage.error());
        let language =
            i18n::scope_async("fr", state.run(|| |_: &mut ()| Ok(i18n::current().code.clone()))).await.unwrap();
        assert_eq!(language, "fr");
        let error = i18n::scope_async("fr", state.run(|| |_: &mut ()| Err::<(), _>(DeliveryProblem::Storage.error())))
            .await
            .unwrap_err();
        assert_eq!(error.detail, Some(ErrorDetail::Delivery(DeliveryProblem::Storage)));
        assert_eq!(error.message, i18n::scope("fr", || DeliveryProblem::Storage.error().message));
        assert_ne!(error.message, i18n::scope("en", || DeliveryProblem::Storage.error().message));
        assert_eq!(i18n::current().code, "en");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_retired_caller_cannot_block_timers_or_let_a_later_operation_overtake_its_worker() {
        let state = BlockingState::new(Vec::<&'static str>::new(), || {
            culprit_core::telemetry::DeliveryProblem::Storage.error()
        });
        let (entered, started) = tokio::sync::oneshot::channel();
        let (release, waiting) = std::sync::mpsc::channel();
        let working = state.clone();
        let caller = tokio::spawn(async move {
            working
                .run(|| {
                    move |values: &mut Vec<&'static str>| {
                        values.push("started");
                        entered.send(()).unwrap();
                        waiting.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
                        values.push("finished");
                        Ok(())
                    }
                })
                .await
        });
        started.await.unwrap();
        caller.abort();
        assert!(caller.await.unwrap_err().is_cancelled());
        let prepared = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let preparation = prepared.clone();
        let next = state.run(|| {
            preparation.store(true, std::sync::atomic::Ordering::Release);
            |values: &mut Vec<&'static str>| Ok(values.clone())
        });
        tokio::pin!(next);
        tokio::select! {
            _=tokio::time::sleep(std::time::Duration::from_millis(5))=>{},
            result=&mut next=>panic!("replacement I/O overtook the running worker: {result:?}"),
        }
        assert!(
            !prepared.load(std::sync::atomic::Ordering::Acquire),
            "queued requests do not allocate their large cloned arguments yet"
        );
        release.send(()).unwrap();
        assert_eq!(next.await.unwrap(), vec!["started", "finished"]);
    }
}
