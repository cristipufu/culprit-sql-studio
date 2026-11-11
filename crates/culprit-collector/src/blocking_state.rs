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
        tokio::task::spawn_blocking(move || operation(&mut self.state)).await.map_err(|_| worker_failure())?
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
