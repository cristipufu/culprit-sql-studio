use super::*;
#[derive(Clone)]
pub(crate) struct Accounting {
    pub(super) budget: CollectionBudget,
    pub(super) deadline: Instant,
    pub(super) queries: Arc<AtomicU32>,
    pub(super) metrics: ReadMetricsRecorder,
}
impl Accounting {
    /// Cancellation remains admitted after an ordinary read budget expires, but its work still appears in cost
    /// diagnostics and query counts. Suppressing cleanup at the limit could leave the database request running.
    pub(crate) fn control_query(&self, sql: &str) -> ControlQuery {
        self.queries.fetch_add(1, Ordering::AcqRel);
        ControlQuery {
            metrics: self.metrics.clone(),
            template: self.metrics.begin(sql),
            started: std::time::Instant::now(),
            progress: ReadProgress::default(),
            finished: false,
        }
    }
    pub(crate) fn new(budget: CollectionBudget) -> Self {
        Self {
            deadline: Instant::now() + Duration::from_millis(budget.total_timeout_ms.into()),
            budget,
            queries: Arc::new(AtomicU32::new(0)),
            metrics: ReadMetricsRecorder::new(Engine::Mysql),
        }
    }
    pub(super) fn remaining(&self) -> Result<Duration> {
        let remaining = self
            .deadline
            .saturating_duration_since(Instant::now())
            .min(Duration::from_millis(self.budget.statement_timeout_ms.into()));
        if remaining.is_zero() { Err(CollectionStop::TimedOut.error()) } else { Ok(remaining) }
    }
    pub(super) fn admit(&self) -> Result<()> {
        self.remaining()?;
        let mut queries = self.queries.load(Ordering::Acquire);
        loop {
            if queries >= self.budget.max_queries {
                return Err(CollectionStop::QueryLimit.error());
            }
            match self.queries.compare_exchange_weak(queries, queries + 1, Ordering::AcqRel, Ordering::Acquire) {
                Ok(_) => return Ok(()),
                Err(current) => queries = current,
            }
        }
    }
}
pub(crate) struct ControlQuery {
    metrics: ReadMetricsRecorder,
    template: Option<String>,
    started: std::time::Instant,
    progress: ReadProgress,
    finished: bool,
}
impl ControlQuery {
    pub(crate) fn complete<T>(mut self, result: &mysql_async::Result<T>, rows: u64, bytes: u64) {
        self.progress.rows.store(rows, Ordering::Release);
        self.progress.bytes.store(bytes, Ordering::Release);
        self.metrics.finish(
            self.template.as_deref(),
            self.started.elapsed(),
            &self.progress,
            &ReadTaskOutcome {
                failed: result.is_err(),
                unconfirmed: !crate::cancellation::response_completed(result),
                ..Default::default()
            },
        );
        self.finished = true;
    }
}
impl Drop for ControlQuery {
    fn drop(&mut self) {
        if !self.finished {
            self.metrics.finish(
                self.template.as_deref(),
                self.started.elapsed(),
                &self.progress,
                &ReadTaskOutcome { failed: true, cancelled: true, unconfirmed: true, ..Default::default() },
            );
        }
    }
}
