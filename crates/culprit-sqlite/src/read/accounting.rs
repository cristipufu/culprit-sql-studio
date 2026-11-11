//! One query budget and native-template accounting span setup, metadata and transaction cleanup.
use culprit_core::{Engine, Result, telemetry::*};
use std::sync::{
    Arc,
    atomic::{AtomicU32, Ordering},
};

#[derive(Clone)]
pub(crate) struct FileAccounting {
    queries: Arc<AtomicU32>,
    maximum: u32,
    metrics: ReadMetricsRecorder,
}
impl FileAccounting {
    pub(crate) fn new(maximum: u32) -> Self {
        Self { queries: Arc::new(AtomicU32::new(0)), maximum, metrics: ReadMetricsRecorder::new(Engine::Sqlite) }
    }
    pub(crate) fn statement<T>(
        &self,
        sql: &str,
        progress: &ReadProgress,
        work: impl FnOnce() -> Result<T>,
    ) -> Result<T> {
        self.queries
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |count| (count < self.maximum).then(|| count + 1))
            .map_err(|_| CollectionStop::QueryLimit.error())?;
        self.record(sql, progress, work)
    }
    // Cleanup still runs when the source has exhausted its query allowance.
    pub(crate) fn cleanup<T>(&self, sql: &str, progress: &ReadProgress, work: impl FnOnce() -> Result<T>) -> Result<T> {
        self.queries.fetch_add(1, Ordering::Relaxed);
        self.record(sql, progress, work)
    }
    fn record<T>(&self, sql: &str, progress: &ReadProgress, work: impl FnOnce() -> Result<T>) -> Result<T> {
        let id = self.metrics.begin(sql);
        let started = std::time::Instant::now();
        let result = work();
        progress.drained.store(result.is_ok(), Ordering::Relaxed);
        self.metrics.finish(
            id.as_deref(),
            started.elapsed(),
            progress,
            &ReadTaskOutcome { failed: result.is_err(), ..Default::default() },
        );
        result
    }
    pub(crate) fn snapshot(&self) -> (u32, Option<ReadWorkMetrics>) {
        (self.queries.load(Ordering::Relaxed), self.metrics.snapshot())
    }
}
