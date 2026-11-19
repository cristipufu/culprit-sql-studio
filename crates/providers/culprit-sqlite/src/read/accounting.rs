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
        let outcome = match result.as_ref().err().and_then(|error| error.detail.as_ref()) {
            Some(culprit_core::ErrorDetail::CollectionStopped(reason)) => ReadTaskOutcome::stopped(*reason),
            _ => ReadTaskOutcome { failed: result.is_err(), ..Default::default() },
        };
        self.metrics.finish(id.as_deref(), started.elapsed(), progress, &outcome);
        result
    }
    pub(crate) fn snapshot(&self) -> (u32, Option<ReadWorkMetrics>) {
        (self.queries.load(Ordering::Relaxed), self.metrics.snapshot())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use culprit_core::{Error, ErrorCode};

    #[test]
    fn explicit_statement_budget_stops_are_retained_without_running_sql() {
        for reason in
            [CollectionStop::ByteLimit, CollectionStop::RowLimit, CollectionStop::TimedOut, CollectionStop::Cancelled]
        {
            let accounting = FileAccounting::new(1);
            let error = accounting
                .statement("SELECT private_owner_budget", &ReadProgress::default(), || Err::<(), _>(reason.error()))
                .unwrap_err();
            assert_eq!(CollectionStop::from_error(&error), reason);
            let (queries, metrics) = accounting.snapshot();
            let metrics = metrics.unwrap();
            assert_eq!(queries, 1);
            assert!(metrics.is_valid());
            assert_eq!(metrics.templates[0].stop_causes, [ReadStopCount { reason, tasks: 1 }]);
            assert_eq!(metrics.templates[0].unconfirmed_cancellations, 0);
        }
    }

    #[test]
    fn native_errors_and_unknown_cancellation_trigger_never_become_original_owner_requests() {
        for error in [
            Error::new(ErrorCode::PermissionDenied, "read only"),
            Error::new(ErrorCode::Timeout, "native busy or interrupted"),
            Error::new(ErrorCode::QueryFailed, "late protocol error"),
            CollectionStop::CancellationUnconfirmed.error(),
        ] {
            let accounting = FileAccounting::new(1);
            accounting
                .statement("SELECT private_native_error", &ReadProgress::default(), || Err::<(), _>(error))
                .unwrap_err();
            let metrics = accounting.snapshot().1.unwrap();
            assert!(metrics.is_valid());
            assert_eq!(metrics.templates[0].failures, 1);
            assert!(metrics.templates[0].stop_causes.is_empty());
        }
    }

    #[test]
    fn cleanup_keeps_the_original_cause_after_the_ordinary_query_allowance_is_spent() {
        let accounting = FileAccounting::new(1);
        accounting.statement("SELECT one", &ReadProgress::default(), || Ok(())).unwrap();
        accounting
            .cleanup("ROLLBACK", &ReadProgress::default(), || Err::<(), _>(CollectionStop::ByteLimit.error()))
            .unwrap_err();
        let (queries, metrics) = accounting.snapshot();
        let metrics = metrics.unwrap();
        assert_eq!(queries, 2);
        assert!(metrics.is_valid());
        let cleanup = metrics.templates.iter().find(|metric| metric.template_id == digest(b"ROLLBACK")).unwrap();
        assert_eq!(cleanup.stop_causes, [ReadStopCount { reason: CollectionStop::ByteLimit, tasks: 1 }]);
    }
}
