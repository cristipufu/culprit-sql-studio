//! One explicitly admitted qualification lifetime. Ordinary sources never construct this reader.
use super::{
    observed_session::{self, PreparedSession, Sample},
    *,
};
use culprit_core::telemetry::*;
use std::sync::Mutex;

/// Shared by both native connections, including their actual session-setup batches.
pub(crate) struct Admission {
    budget: CollectionBudget,
    deadline: Instant,
    state: Mutex<Consumption>,
}
#[derive(Default)]
struct Consumption {
    queries: u32,
    bytes: u64,
    stopped: Option<CollectionStop>,
}
impl Admission {
    fn new(budget: CollectionBudget) -> Result<Self> {
        budget.validate()?;
        Ok(Self {
            deadline: Instant::now() + Duration::from_millis(budget.total_timeout_ms.into()),
            budget,
            state: Mutex::new(Consumption::default()),
        })
    }
    pub(super) fn start(&self) -> std::result::Result<(), CollectionStop> {
        let mut state = self.state.lock().map_err(|_| CollectionStop::CancellationUnconfirmed)?;
        if let Some(stop) = state.stopped {
            return Err(stop);
        }
        let stop = if Instant::now() >= self.deadline {
            Some(CollectionStop::TimedOut)
        } else if state.queries >= self.budget.max_queries {
            Some(CollectionStop::QueryLimit)
        } else {
            None
        };
        if let Some(stop) = stop {
            state.stopped = Some(stop);
            return Err(stop);
        }
        state.queries += 1;
        Ok(())
    }
    pub(super) fn charge(&self, bytes: usize) -> std::result::Result<(), CollectionStop> {
        let mut state = self.state.lock().map_err(|_| CollectionStop::CancellationUnconfirmed)?;
        if let Some(stop) = state.stopped {
            return Err(stop);
        }
        let Some(total) = state.bytes.checked_add(bytes as u64) else {
            state.stopped = Some(CollectionStop::ByteLimit);
            return Err(CollectionStop::ByteLimit);
        };
        state.bytes = total;
        if state.bytes > u64::from(self.budget.total_bytes) {
            state.stopped = Some(CollectionStop::ByteLimit);
            return Err(CollectionStop::ByteLimit);
        }
        Ok(())
    }
    fn stop(&self, reason: CollectionStop) {
        if let Ok(mut state) = self.state.lock() {
            state.stopped.get_or_insert(reason);
        }
    }
    fn remaining(&self) -> Result<()> {
        let state = self.state.lock().map_err(|_| CollectionStop::CancellationUnconfirmed.error())?;
        if let Some(stop) = state.stopped {
            return Err(stop.error());
        }
        if Instant::now() >= self.deadline {
            return Err(CollectionStop::TimedOut.error());
        }
        Ok(())
    }
}

/// The existing Pool calls this only immediately before its real SET batch.
pub(crate) struct SetupWork {
    admission: Option<Arc<Admission>>,
    metrics: crate::read_metrics::Metrics,
    queries: std::sync::atomic::AtomicU32,
}
impl SetupWork {
    fn new(admission: Arc<Admission>) -> Self {
        Self {
            admission: Some(admission),
            metrics: crate::read_metrics::Metrics::new(culprit_core::Engine::Mssql),
            queries: Default::default(),
        }
    }
    pub(crate) fn observation() -> Self {
        Self {
            admission: None,
            metrics: crate::read_metrics::Metrics::new(culprit_core::Engine::Mssql),
            queries: Default::default(),
        }
    }
    pub(crate) fn snapshot(&self) -> (u32, Option<culprit_core::telemetry::ReadWorkMetrics>) {
        (self.queries.load(std::sync::atomic::Ordering::Acquire), self.metrics.snapshot())
    }
    pub(crate) fn begin(&self, sql: &str) -> Result<SetupTask<'_>> {
        if let Some(admission) = &self.admission {
            admission.start().map_err(CollectionStop::error)?;
        }
        self.queries.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        let sql = crate::statement_origin::requested_batch(sql);
        Ok(SetupTask {
            work: self,
            template: self.metrics.begin(&sql),
            started: std::time::Instant::now(),
            finished: false,
        })
    }
}
pub(crate) struct SetupTask<'a> {
    work: &'a SetupWork,
    template: Option<String>,
    started: std::time::Instant,
    finished: bool,
}
impl SetupTask<'_> {
    pub(crate) fn finish(mut self, success: bool) {
        self.record(success);
    }
    fn record(&mut self, success: bool) {
        self.work.metrics.finish(
            self.template.as_deref(),
            self.started.elapsed(),
            &crate::read_metrics::Progress::default(),
            &culprit_core::telemetry::ReadTaskOutcome { failed: !success, unconfirmed: !success, ..Default::default() },
        );
        if !success && let Some(admission) = &self.work.admission {
            admission.stop(CollectionStop::CancellationUnconfirmed);
        }
        self.finished = true;
    }
}
impl Drop for SetupTask<'_> {
    fn drop(&mut self) {
        if !self.finished {
            self.record(false);
        }
    }
}

pub(crate) trait QualificationSession: NativeStreamRead + Sized {
    fn accounting(&self) -> Option<ReadPhaseAccounting>;
    fn ready_for_transfer(&self) -> Result<()>;
    fn into_source_phase(self) -> Self;
    fn session_work(&mut self) -> &mut session_work::SessionWork;
}
impl ReadSession {
    fn qualification(pool: Arc<Pool>, admission: Arc<Admission>) -> Self {
        let mut owner = Self::new(pool, admission.budget.clone());
        owner.connection_work = Some(Default::default());
        owner.deadline = admission.deadline;
        owner.session_setup = Some(SetupWork::new(admission.clone()));
        owner.admission = Some(admission);
        owner
    }
}
impl QualificationSession for ReadSession {
    fn accounting(&self) -> Option<ReadPhaseAccounting> {
        Some(ReadPhaseAccounting {
            queries: self.queries(),
            metrics: Box::new(auxiliary_metrics(self.read_metrics()?)?),
            session_setup: Box::new(auxiliary_metrics(match &self.session_setup {
                Some(setup) => setup.metrics.snapshot()?,
                None => empty_metrics(),
            })?),
        })
    }
    fn ready_for_transfer(&self) -> Result<()> {
        let accounting = self.accounting().ok_or_else(invalid_data)?;
        if self.client.is_none()
            || self.active.is_some()
            || self.streaming.is_some()
            || self.acquiring
            || !self.cancellation_confirmed
            || accounting.metrics.active_tasks != 0
            || accounting.metrics.untracked_tasks != 0
            || accounting.metrics.has_unconfirmed_work()
            || matches!(accounting.metrics.native_session_work.as_deref(), Some(NativeSessionReadWork::MssqlSession(work)) if work.issue.is_some())
            || accounting.metrics.templates.iter().any(|template| template.failures > 0)
            || accounting.session_setup.has_unconfirmed_work()
            || accounting.session_setup.templates.iter().any(|template| template.failures > 0)
        {
            return Err(CollectionStop::CancellationUnconfirmed.error());
        }
        self.pool.ensure_available()?;
        self.admission.as_ref().ok_or_else(invalid_data)?.remaining()
    }
    fn into_source_phase(mut self) -> Self {
        // Preparation accounting is sealed before this consumed transition. Only phase-local counters reset;
        // the client, acquisition history, pending first-query token and parent deadline keep their owner.
        self.queries = 0;
        self.metrics = crate::read_metrics::Metrics::new(culprit_core::Engine::Mssql);
        self.native_work = session_work::SessionWork::default();
        self.session_setup = None;
        self
    }
    fn session_work(&mut self) -> &mut session_work::SessionWork {
        &mut self.native_work
    }
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Stage {
    Unprepared,
    Preparing,
    Measuring,
    Finishing,
    Finished,
    Failed,
    Closed,
}
pub(crate) struct QualificationRead<R: QualificationSession = ReadSession> {
    owner: Option<R>,
    observer: R,
    admission: Arc<Admission>,
    host: String,
    port: u16,
    prepared: Option<PreparedSession>,
    preparation: Option<ReadPhaseAccounting>,
    stage: Stage,
    shutdown: Option<SourceShutdown>,
}
impl QualificationRead {
    pub(crate) fn new(pool: Arc<Pool>, budget: CollectionBudget, host: String, port: u16) -> Result<Self> {
        let admission = Arc::new(Admission::new(budget)?);
        let source = ReadSession::qualification(pool.clone(), admission.clone());
        let observer = ReadSession::qualification(pool, admission.clone());
        Ok(Self::with_sessions(source, observer, admission, host, port))
    }
}
impl<R: QualificationSession> QualificationRead<R> {
    fn with_sessions(owner: R, observer: R, admission: Arc<Admission>, host: String, port: u16) -> Self {
        Self {
            owner: Some(owner),
            observer,
            admission,
            host,
            port,
            prepared: None,
            preparation: None,
            stage: Stage::Unprepared,
            shutdown: None,
        }
    }
    fn owner(&mut self) -> ReadResult<&mut R> {
        self.owner.as_mut().ok_or(ReadError::Stopped(CollectionStop::CancellationUnconfirmed))
    }
    async fn start(&mut self) -> ReadResult<()> {
        if self.stage == Stage::Measuring {
            return Ok(());
        }
        if self.stage != Stage::Unprepared {
            return Err(ReadError::Stopped(CollectionStop::CancellationUnconfirmed));
        }
        self.stage = Stage::Preparing;
        let result = self.prepare().await;
        if result.is_err() {
            self.stage = Stage::Failed;
        }
        result.map_err(ReadError::Source)
    }
    async fn prepare(&mut self) -> Result<()> {
        self.admission.remaining()?;
        let source = self.owner.as_mut().ok_or_else(invalid_data)?;
        let prepared =
            observed_session::prepare(source, &self.host, self.port, crate::activity::IdentityDetail::Endpoint).await?;
        if prepared.authority.activity.edition != crate::probe::ENGINE_EDITION_AZURE_SQL_DATABASE {
            return Err(invalid_data());
        }
        require_primary(prepared.context.database_writable)?;
        NativeSourceReadScope::from_context(&prepared.context)?;
        source.ready_for_transfer()?;
        observed_session::observer_scope(&mut self.observer, &prepared.context, &self.host, self.port).await?;
        let before =
            observed_session::sample(&mut self.observer, &prepared.identity).await?.ok_or_else(invalid_data)?;
        if before.active {
            return Err(CollectionStop::SourceChanged.error());
        }
        source.ready_for_transfer()?;
        let accounting = source.accounting().ok_or_else(invalid_data)?;
        let source = self.owner.take().ok_or_else(invalid_data)?;
        self.owner = Some(source.into_source_phase());
        self.preparation = Some(accounting);
        self.prepared = Some(prepared);
        self.boundary(&before, 0)?;
        self.stage = Stage::Measuring;
        Ok(())
    }
    fn boundary(&mut self, sample: &Sample, ordinal: u32) -> Result<()> {
        let prepared = self.prepared.as_ref().ok_or_else(invalid_data)?;
        if sample.active || ObservationBounds::new(&prepared.context.observed_until, &sample.observed_at).is_none() {
            return Err(CollectionStop::SourceChanged.error());
        }
        let mut authority = prepared.authority.clone();
        authority.activity.at = sample.observed_at.clone();
        let session = NativeActivityIdentity {
            id: prepared.identity.id.to_string(),
            generation: prepared.identity.login.clone(),
        };
        let source = self.owner.as_mut().ok_or_else(invalid_data)?;
        let generation = source.connection_generation().ok_or_else(invalid_data)?;
        source.session_work().source_boundary(
            &authority,
            session,
            MssqlSessionReadPoint {
                query_ordinal: ordinal,
                cpu_ms: Some(sample.counters.cpu_ms.to_string()),
                reads: Some(sample.counters.reads.to_string()),
                logical_reads: Some(sample.counters.logical_reads.to_string()),
                dirty_marks: Some(sample.counters.dirty_marks.to_string()),
                memory_pages: sample.memory_pages.map(|value| value.to_string()),
            },
            generation,
        );
        let metrics = source.read_metrics().ok_or_else(invalid_data)?;
        let Some(NativeSessionReadWork::MssqlSession(work)) = metrics.native_session_work.as_deref() else {
            return Err(invalid_data());
        };
        if work.issue.is_some() {
            return Err(invalid_data());
        }
        Ok(())
    }
    fn result<T>(&mut self, result: ReadResult<T>) -> ReadResult<T> {
        if result.is_err() {
            self.stage = Stage::Failed;
        }
        result
    }
    async fn final_boundary(&mut self) -> Result<()> {
        self.admission.remaining()?;
        self.owner.as_ref().ok_or_else(invalid_data)?.ready_for_transfer()?;
        let prepared = self.prepared.as_ref().ok_or_else(invalid_data)?;
        observed_session::observer_scope(&mut self.observer, &prepared.context, &self.host, self.port).await?;
        self.observer.ready_for_transfer()?;
        let after = observed_session::sample(&mut self.observer, &prepared.identity).await?.ok_or_else(invalid_data)?;
        self.boundary(&after, self.queries())
    }
}
// Reuse the existing SQL-preview allowlist without changing any original accounting or native proof.
// Repeated catalog SQL is identified by its exact template hash; debug SQL/parameter traces stay test-only.
fn auxiliary_metrics(mut metrics: ReadWorkMetrics) -> Option<ReadWorkMetrics> {
    metrics.templates = metrics.summary(None).ok()?.templates;
    metrics.is_valid().then_some(metrics)
}
fn empty_metrics() -> ReadWorkMetrics {
    ReadWorkMetrics {
        connection_work: None,
        active_tasks: 0,
        untracked_tasks: 0,
        templates: vec![],
        native_session_work: None,
    }
}
#[async_trait::async_trait]
impl<R: QualificationSession> NativeRead for QualificationRead<R> {
    async fn query(&mut self, sql: &str, parameters: Vec<Parameter>, limit: usize) -> ReadResult<Vec<Rec>> {
        self.start().await?;
        let result = self.owner()?.query(sql, parameters, limit).await;
        self.result(result)
    }
    fn queries(&self) -> u32 {
        if self.preparation.is_some() { self.owner.as_ref().map_or(0, NativeRead::queries) } else { 0 }
    }
    fn connection_generation(&self) -> Option<u64> {
        self.owner.as_ref().and_then(NativeRead::connection_generation)
    }
    fn observe_session_work(&mut self, observation: session_work::Observation) {
        if self.stage == Stage::Measuring {
            if let Some(owner) = &mut self.owner {
                owner.observe_session_work(observation);
            }
        }
    }
    fn read_metrics(&self) -> Option<ReadWorkMetrics> {
        let preparation = match &self.preparation {
            Some(preparation) => preparation.clone(),
            None => self.owner.as_ref()?.accounting()?,
        };
        let mut metrics =
            if self.preparation.is_some() { self.owner.as_ref()?.read_metrics()? } else { empty_metrics() };
        let native = metrics.native_session_work.get_or_insert_with(|| {
            Box::new(NativeSessionReadWork::MssqlSession(Box::new(MssqlSessionReadWork {
                qualification_work: None,
                settlement: NativeReadSettlement::Pending,
                issue: Some(NativeReadWorkIssue::MissingSession),
                interval: None,
            })))
        });
        let NativeSessionReadWork::MssqlSession(work) = native.as_mut();
        if self.shutdown == Some(SourceShutdown::Unconfirmed) {
            work.settlement = NativeReadSettlement::Unconfirmed;
        }
        work.qualification_work = Some(Box::new(MssqlQualificationReadWork {
            aggregate_budget: self.admission.budget.clone(),
            preparation,
            observer: self.observer.accounting()?,
        }));
        Some(metrics)
    }
    async fn finish_measurement(&mut self) -> ReadResult<()> {
        if self.stage != Stage::Measuring {
            self.stage = Stage::Failed;
            if let Some(source) = &mut self.owner {
                source.session_work().poison(NativeReadWorkIssue::InvalidObservation);
            }
            return Err(ReadError::Source(invalid_data()));
        }
        self.stage = Stage::Finishing;
        let result = self.final_boundary().await;
        self.stage = if result.is_ok() { Stage::Finished } else { Stage::Failed };
        result.map_err(ReadError::Source)
    }
    async fn close(&mut self) -> SourceShutdown {
        if let Some(shutdown) = self.shutdown {
            return shutdown;
        }
        if self.stage != Stage::Finished {
            if let Some(owner) = &mut self.owner {
                owner.session_work().poison(NativeReadWorkIssue::MissingFinalObservation);
            }
        }
        // Close owns no SQL. Interrupted preparation/finish cannot be resumed or repaired by cleanup.
        let source = match &mut self.owner {
            Some(owner) => owner.close().await,
            None => SourceShutdown::Unconfirmed,
        };
        let observer = self.observer.close().await;
        let shutdown = if source == SourceShutdown::Unconfirmed || observer == SourceShutdown::Unconfirmed {
            SourceShutdown::Unconfirmed
        } else {
            SourceShutdown::Quiescent
        };
        self.shutdown = Some(shutdown);
        self.stage = Stage::Closed;
        shutdown
    }
}
#[async_trait::async_trait]
impl<R: QualificationSession> NativeStreamRead for QualificationRead<R> {
    async fn begin_stream(&mut self, sql: &str, parameters: Vec<Parameter>, limit: usize) -> ReadResult<()> {
        self.start().await?;
        let result = self.owner()?.begin_stream(sql, parameters, limit).await;
        self.result(result)
    }
    async fn next_batch(&mut self) -> ReadResult<ReadBatch> {
        if self.stage != Stage::Measuring {
            return Err(ReadError::Stopped(CollectionStop::CancellationUnconfirmed));
        }
        let result = self.owner()?.next_batch().await;
        self.result(result)
    }
}
#[cfg(test)]
mod tests;
#[cfg(test)]
pub(crate) use tests::{ScriptedSession, scripted_reader};
