//! Owned-fixture instrumentation around the real reader. Observer and preparation work are separate from source work.
use super::*;
use crate::{activity::IdentityDetail, local_source};
use culprit_core::telemetry::*;
use serde_json::{Value, json};
use std::sync::atomic::{AtomicBool, Ordering};

const OBSERVER_TIMEOUT: Duration = Duration::from_secs(3);
const SAMPLE_INTERVAL: Duration = Duration::from_millis(10);
const MAX_PHASES: usize = 32;
const MAX_OBSERVER_QUERIES: u32 = 512;
const OBSERVER_TOTAL_TIMEOUT_MS: u32 = 60_000;
mod lifecycle;
mod observation;
mod result_trace;
pub(crate) use lifecycle::{assert_no_followup, lifecycle_cases};
use observation::{await_observed, measured_result, settled_sample};
use result_trace::ResultTrace;

#[derive(Clone)]
pub(crate) struct TemplateObservation {
    id: String,
    observed: Arc<AtomicBool>,
}
impl TemplateObservation {
    pub(crate) fn was_active(&self) -> bool {
        self.observed.load(Ordering::Acquire)
    }
    fn observe(&self, sql: &str, active: bool) {
        if active && digest(sql.as_bytes()) == self.id {
            self.observed.store(true, Ordering::Release);
        }
    }
}

#[derive(Clone)]
struct SessionIdentity {
    id: i64,
    login: String,
}
// These native meanings match MssqlSessionReadPoint: reads are counts and writes are dirty marks.
#[derive(Clone)]
struct Counters {
    cpu_ms: u64,
    logical_reads: u64,
    reads: u64,
    dirty_marks: u64,
}
struct Sample {
    observed_at: String,
    counters: Counters,
    memory_pages: Option<u64>,
    active: bool,
    wait: Option<String>,
    blocker: Option<i64>,
    granted_kb: Option<u64>,
    requested_kb: Option<u64>,
    used_kb: Option<u64>,
}
/// A separately acquired observer keeps its own route and setup cost. Fixture admission belongs to its caller.
pub(crate) struct HeldObserver {
    owner: ReadSession,
    setup_elapsed_ms: f64,
    scope: Option<NativeReadContext>,
}
impl HeldObserver {
    pub(crate) async fn acquire(pool: Arc<Pool>) -> culprit_core::Result<Self> {
        let started = Instant::now();
        let mut owner = ReadSession::new(pool, observer_budget());
        let client = owner.acquire(OBSERVER_TIMEOUT).await.map_err(ReadError::into_error)?;
        owner.client = Some(client);
        Ok(Self { owner, setup_elapsed_ms: started.elapsed().as_secs_f64() * 1000.0, scope: None })
    }
    async fn qualify(&mut self, source: &NativeReadContext, host: &str, port: u16) -> culprit_core::Result<()> {
        let started = Instant::now();
        let result = observer_scope(&mut self.owner, source, host, port).await;
        self.setup_elapsed_ms += started.elapsed().as_secs_f64() * 1000.0;
        self.scope = Some(result?);
        Ok(())
    }
}

fn observer_budget() -> CollectionBudget {
    CollectionBudget {
        max_queries: MAX_OBSERVER_QUERIES,
        statement_timeout_ms: OBSERVER_TIMEOUT.as_millis() as u32,
        total_timeout_ms: OBSERVER_TOTAL_TIMEOUT_MS,
        ..Default::default()
    }
}

async fn observer_scope<R: NativeRead>(
    observer: &mut R,
    source: &NativeReadContext,
    host: &str,
    port: u16,
) -> culprit_core::Result<NativeReadContext> {
    let authority = local_source::Context::read(observer, host, port).await?;
    let context = authority.completed(&authority)?;
    if !context.matches_earlier_scope(source) {
        return Err(CollectionStop::SourceChanged.error());
    }
    Ok(context)
}
struct Observer {
    connection: HeldObserver,
    identity: SessionIdentity,
    queries: u32,
    elapsed_ms: f64,
    charged_bytes: u64,
    failed: bool,
}
struct Phase {
    sql: String,
    parameters: Value,
    row_limit: usize,
    before: Counters,
    started: Instant,
    active_samples: u32,
    waits: std::collections::BTreeSet<String>,
    blocking_samples: u32,
    maximum_sampled_granted_kb: Option<u64>,
    maximum_sampled_requested_kb: Option<u64>,
    maximum_sampled_used_kb: Option<u64>,
    observer_error: Option<Error>,
}

pub(crate) struct MeasuredRead {
    owner: ReadSession,
    observer: Observer,
    generation: u64,
    context: NativeReadContext,
    authority: local_source::Context,
    preparation: Value,
    phase: Option<Phase>,
    phases: Vec<Value>,
    active_seen: Arc<AtomicBool>,
    template_watch: Option<TemplateObservation>,
    result_trace: Option<ResultTrace>,
    errors: Vec<String>,
    closing: bool,
    final_sample_started: bool,
    closed: bool,
}

impl MeasuredRead {
    pub(crate) async fn open(
        pool: Arc<Pool>,
        host: &str,
        port: u16,
        mut observer: HeldObserver,
        budget: CollectionBudget,
    ) -> culprit_core::Result<Self> {
        budget.validate()?;
        let mut owner = ReadSession::new(
            pool,
            CollectionBudget { max_queries: 16, total_timeout_ms: 10_000, ..Default::default() },
        );
        let PreparedSource { identity, context, authority, report: preparation } =
            prepare_owned(&mut owner, host, port).await?;
        if let Err(error) = observer.qualify(&context, host, port).await {
            let source_shutdown = owner.close().await;
            let _ = observer.owner.close().await;
            return Err(if source_shutdown == SourceShutdown::Unconfirmed {
                CollectionStop::CancellationUnconfirmed.error()
            } else {
                error
            });
        }
        let observer = Observer {
            queries: observer.owner.queries(),
            connection: observer,
            identity,
            elapsed_ms: 0.0,
            charged_bytes: 0,
            failed: false,
        };
        let generation = owner.connection_generation;
        if let Err(error) = owner.measurement_window(budget) {
            let shutdown = owner.close().await;
            eprintln!("cost measurement admission failed: code={:?},shutdown={shutdown:?}", error.code);
            return Err(error);
        }
        Ok(Self {
            owner,
            observer,
            generation,
            context,
            authority,
            preparation,
            phase: None,
            phases: vec![],
            active_seen: Arc::new(AtomicBool::new(false)),
            template_watch: None,
            result_trace: None,
            errors: vec![],
            closing: false,
            final_sample_started: false,
            closed: false,
        })
    }

    pub(crate) fn active_seen(&self) -> Arc<AtomicBool> {
        self.active_seen.clone()
    }
    pub(crate) fn scope(&self) -> &NativeReadContext {
        &self.context
    }
    pub(crate) fn watch_template(&mut self, id: &str) -> culprit_core::Result<TemplateObservation> {
        if self.owner.queries() != 0 || self.phase.is_some() || self.template_watch.is_some() || !valid_digest(id) {
            return Err(invalid_data());
        }
        let observation = TemplateObservation { id: id.into(), observed: Arc::new(AtomicBool::new(false)) };
        self.template_watch = Some(observation.clone());
        Ok(observation)
    }
    /// Opt in before the source window. This test-only trace retains one bounded original result and never
    /// changes query execution, source errors, observer accounting or default response retention.
    pub(crate) fn trace_result(&mut self, template_id: &str) -> culprit_core::Result<()> {
        if self.owner.queries() != 0
            || self.phase.is_some()
            || self.result_trace.is_some()
            || !valid_digest(template_id)
        {
            return Err(invalid_data());
        }
        self.result_trace = Some(ResultTrace::new(template_id));
        Ok(())
    }
    pub(crate) fn take_result(&mut self) -> culprit_core::Result<Option<Vec<Rec>>> {
        self.result_trace.as_mut().map_or(Ok(None), ResultTrace::take)
    }
    pub(crate) async fn is_quiet(&mut self) -> culprit_core::Result<bool> {
        self.observer.is_quiet().await
    }
    pub(crate) fn report(&self) -> Value {
        json!({
            "preparation":self.preparation,
            "source_read_metrics":self.owner.read_metrics(),
            "source_queries":self.owner.queries(),
            "phases":self.phases,
            "observer":{"setup_client_elapsed_ms":self.observer.connection.setup_elapsed_ms,"queries":self.observer.connection.owner.queries(),"attempts_including_scope":self.observer.queries,"client_elapsed_ms":self.observer.elapsed_ms,"charged_value_bytes":self.observer.charged_bytes.to_string(),"scope":self.observer.connection.scope,"read_metrics":self.observer.connection.owner.read_metrics()},
            "grant_evidence":"sampled-current-grants-only; missed samples do not establish no grant or a peak",
            "measurement_errors":self.errors,
            "measurement_complete":self.measurement_complete(),
            "native_cpu_io_cost_qualified":false,
            "measurement_scope":"all original capture SQL after separately charged preparation; excludes login, session setup and cold-start cost",
        })
    }

    fn measurement_complete(&self) -> bool {
        self.closed
            && self.errors.is_empty()
            && self.phase.is_none()
            && self.phases.len() == self.owner.queries() as usize
            && self.owner.read_metrics().is_some_and(|metrics| {
                metrics.is_valid()
                    && metrics.active_tasks == 0
                    && metrics.untracked_tasks == 0
                    && matches!(metrics.native_session_work.as_deref(), Some(NativeSessionReadWork::MssqlSession(work))
                        if work.settlement == NativeReadSettlement::Quiescent
                            && work.issue.is_none()
                            && work.interval.as_ref().is_some_and(|interval|
                                interval.initial.query_ordinal == 0
                                && interval.latest.as_ref().is_some_and(|point| point.query_ordinal == self.owner.queries())))
            })
    }

    fn record_boundary(&mut self, sample: &Sample, ordinal: u32) {
        if ObservationBounds::new(&self.context.observed_until, &sample.observed_at).is_none() {
            self.owner.native_work.poison(NativeReadWorkIssue::ClockChanged);
            return;
        }
        let mut authority = self.authority.clone();
        authority.activity.at = sample.observed_at.clone();
        self.owner.native_work.source_boundary(
            &authority,
            NativeActivityIdentity {
                id: self.observer.identity.id.to_string(),
                generation: self.observer.identity.login.clone(),
            },
            MssqlSessionReadPoint {
                query_ordinal: ordinal,
                cpu_ms: Some(sample.counters.cpu_ms.to_string()),
                reads: Some(sample.counters.reads.to_string()),
                logical_reads: Some(sample.counters.logical_reads.to_string()),
                dirty_marks: Some(sample.counters.dirty_marks.to_string()),
                memory_pages: sample.memory_pages.map(|value| value.to_string()),
            },
            self.owner.connection_generation,
        );
    }

    async fn begin(&mut self, sql: &str, parameters: &[Parameter], row_limit: usize) -> culprit_core::Result<()> {
        if self.closing || self.phase.is_some() || self.phases.len() == MAX_PHASES {
            return Err(invalid_data());
        }
        let before = match self.observer.sample().await {
            Ok(sample) => sample,
            Err(error) => {
                self.errors.push(format!("observer-before:{:?}", error.code));
                return Err(error);
            }
        };
        if before.active {
            return Err(invalid_data());
        }
        if self.owner.queries() == 0 {
            self.record_boundary(&before, 0);
        }
        self.phase = Some(Phase {
            sql: sql.into(),
            parameters: parameter_values(parameters),
            row_limit,
            before: before.counters,
            started: Instant::now(),
            active_samples: 0,
            waits: Default::default(),
            blocking_samples: 0,
            maximum_sampled_granted_kb: None,
            maximum_sampled_requested_kb: None,
            maximum_sampled_used_kb: None,
            observer_error: None,
        });
        Ok(())
    }
    async fn end(&mut self, outcome: &str) -> culprit_core::Result<()> {
        let after = match self.phase.as_ref().and_then(|phase| phase.observer_error.clone()) {
            Some(error) => Err(error),
            None => self.observer.sample().await,
        };
        let phase = self.phase.take().ok_or_else(invalid_data)?;
        let sample = match after {
            Ok(sample) => sample,
            Err(error) => {
                self.errors.push(format!("{:?}", error.code));
                self.phases.push(json!({
                    "template_id":digest(phase.sql.as_bytes()),
                    "original_sql":phase.sql,
                    "parameters":phase.parameters,
                    "row_limit":phase.row_limit,
                    "outcome":outcome,
                    "native_delta":null,
                    "measurement_error":error.code,
                }));
                return Err(error);
            }
        };
        let (native, measurement_error) =
            match qualified_delta(&phase.before, &sample.counters, self.generation, self.owner.connection_generation) {
                Ok(native) => (native, None),
                Err(error) => {
                    self.errors.push(format!("native-delta:{:?}", error.code));
                    (Value::Null, Some(error))
                }
            };
        self.phases.push(json!({
            "template_id":digest(phase.sql.as_bytes()),
            "original_sql":phase.sql,
            "parameters":phase.parameters,
            "row_limit":phase.row_limit,
            "client_elapsed_ms":phase.started.elapsed().as_secs_f64()*1000.0,
            "native_delta":native,
            "measurement_error":measurement_error.as_ref().map(|error|error.code),
            "outcome":outcome,
            "active_at_last_observation":sample.active,
            "active_samples":phase.active_samples,
            "wait_types":phase.waits,
            "positive_native_blocker_id_samples":phase.blocking_samples,
            "maximum_sampled_granted_kb":phase.maximum_sampled_granted_kb,
            "maximum_sampled_requested_kb":phase.maximum_sampled_requested_kb,
            "maximum_sampled_used_kb":phase.maximum_sampled_used_kb,
            "grant_not_observed":phase.maximum_sampled_granted_kb.is_none(),
        }));
        if let Some(error) = measurement_error {
            return Err(error);
        }
        Ok(())
    }

    async fn complete_phase<T>(&mut self, result: ReadResult<T>, outcome: &str) -> ReadResult<T> {
        // Losing the observer never drops an in-flight native operation. Callers first await that same
        // bounded operation, then close its retained owner before completing a successful unmeasured statement.
        let shutdown = if self.phase.as_ref().is_some_and(|phase| phase.observer_error.is_some()) {
            self.owner.native_work.poison(NativeReadWorkIssue::MissingFinalObservation);
            Some(self.owner.close().await)
        } else {
            None
        };
        let accounting = self.end(outcome).await;
        measured_result(result, accounting, shutdown)
    }
}

impl ReadSession {
    // Only a test may separate already-recorded preparation from an unchanged source budget. The held socket,
    // connection generation and uncertainty fence remain owned; active or uncertain work cannot be reset.
    fn measurement_window(&mut self, budget: CollectionBudget) -> culprit_core::Result<()> {
        budget.validate()?;
        let settled = self.read_metrics().is_some_and(|metrics| {
            !metrics.has_unconfirmed_work()
                && metrics.untracked_tasks == 0
                && metrics.templates.iter().all(|phase| phase.failures == 0)
        });
        if self.client.is_none()
            || self.active.is_some()
            || self.streaming.is_some()
            || self.acquiring
            || !self.cancellation_confirmed
            || !settled
            || self.deadline <= Instant::now()
            || self.queries >= self.budget.max_queries
        {
            return Err(invalid_data());
        }
        self.pool.ensure_available()?;
        self.budget = budget;
        self.deadline = Instant::now() + Duration::from_millis(self.budget.total_timeout_ms.into());
        self.queries = 0;
        self.metrics = crate::read_metrics::Metrics::new(culprit_core::Engine::Mssql);
        self.native_work = super::session_work::SessionWork::default();
        Ok(())
    }
}

#[async_trait::async_trait]
impl NativeRead for MeasuredRead {
    fn observe_session_work(&mut self, observation: super::session_work::Observation) {
        self.owner.observe_session_work(observation);
    }
    async fn query(&mut self, sql: &str, parameters: Vec<Parameter>, row_limit: usize) -> ReadResult<Vec<Rec>> {
        self.begin(sql, &parameters, row_limit).await.map_err(ReadError::Source)?;
        let phase = self.phase.as_mut().ok_or_else(|| ReadError::Source(invalid_data()))?;
        let result = await_observed(
            self.owner.query(sql, parameters, row_limit),
            &mut self.observer,
            phase,
            &self.active_seen,
            self.template_watch.as_ref(),
        )
        .await;
        let outcome = match &result {
            Ok(_) => "completed",
            Err(ReadError::Source(_)) => "source-error",
            Err(ReadError::Stopped(_)) => "stopped",
        };
        let rows = self.complete_phase(result, outcome).await?;
        if let Some(trace) = &mut self.result_trace {
            trace.retain(sql, &rows);
        }
        Ok(rows)
    }
    fn queries(&self) -> u32 {
        self.owner.queries()
    }
    fn connection_generation(&self) -> Option<u64> {
        Some(self.owner.connection_generation)
    }
    fn read_metrics(&self) -> Option<ReadWorkMetrics> {
        self.owner.read_metrics()
    }
    async fn close(&mut self) -> SourceShutdown {
        self.closing = true;
        if self.closed {
            return self.owner.close().await;
        }
        self.owner.settle_native().await;
        if self.phase.is_some() {
            let _ = self.end("interrupted-before-completion").await;
        }
        if !self.final_sample_started {
            self.final_sample_started = true;
            match settled_sample(&mut self.owner, &mut self.observer).await {
                Ok(sample) if !sample.active => self.record_boundary(&sample, self.owner.queries()),
                Ok(_) => {
                    self.errors.push("original-session-still-active".into());
                    self.owner.native_work.poison(NativeReadWorkIssue::MissingFinalObservation);
                }
                Err(error) => {
                    self.errors.push(format!("observer-final:{:?}", error.code));
                    self.owner.native_work.poison(NativeReadWorkIssue::MissingFinalObservation);
                }
            }
        } else {
            self.owner.native_work.poison(NativeReadWorkIssue::MissingFinalObservation);
        }
        // Later independent quiet checks keep this same held observer. Settlement joins its original native
        // task without returning the socket to a pool or permitting an unnoticed replacement connection.
        self.observer.connection.owner.settle_native().await;
        let observer_shutdown = self.observer.connection.owner.pool.source_shutdown();
        if observer_shutdown == SourceShutdown::Unconfirmed {
            self.errors.push("observer-shutdown-unconfirmed".into());
        }
        let shutdown = self.owner.close().await;
        self.closed = true;
        shutdown
    }
}

#[async_trait::async_trait]
impl NativeStreamRead for MeasuredRead {
    async fn begin_stream(&mut self, sql: &str, parameters: Vec<Parameter>, row_limit: usize) -> ReadResult<()> {
        self.begin(sql, &parameters, row_limit).await.map_err(ReadError::Source)?;
        let phase = self.phase.as_mut().ok_or_else(|| ReadError::Source(invalid_data()))?;
        let result = await_observed(
            self.owner.begin_stream(sql, parameters, row_limit),
            &mut self.observer,
            phase,
            &self.active_seen,
            self.template_watch.as_ref(),
        )
        .await;
        // A successful start does not settle the statement. Keep its phase (including any observer loss)
        // until the same stream reaches its native terminal result or the retained owner is closed.
        if result.is_err() {
            return self.complete_phase(result, "stream-start-failed").await;
        }
        result
    }
    async fn next_batch(&mut self) -> ReadResult<ReadBatch> {
        let phase = self.phase.as_mut().ok_or_else(|| ReadError::Source(invalid_data()))?;
        let result = await_observed(
            self.owner.next_batch(),
            &mut self.observer,
            phase,
            &self.active_seen,
            self.template_watch.as_ref(),
        )
        .await;
        // A partial batch cannot replace a later native byte/row/server error with instrumentation failure.
        if result.as_ref().is_ok_and(|batch| batch.complete) || result.is_err() {
            let outcome = match &result {
                Ok(_) => "stream-completed",
                Err(ReadError::Source(_)) => "source-error",
                Err(ReadError::Stopped(_)) => "stopped",
            };
            return self.complete_phase(result, outcome).await;
        }
        result
    }
}

impl Observer {
    async fn sample(&mut self) -> culprit_core::Result<Sample> {
        self.sample_optional().await?.ok_or_else(|| CollectionStop::SourceChanged.error())
    }
    async fn is_quiet(&mut self) -> culprit_core::Result<bool> {
        Ok(self.sample_optional().await?.is_none_or(|sample| !sample.active))
    }
    async fn sample_optional(&mut self) -> culprit_core::Result<Option<Sample>> {
        if self.failed {
            return Err(CollectionStop::CancellationUnconfirmed.error());
        }
        if self.queries == MAX_OBSERVER_QUERIES {
            return Err(CollectionStop::QueryLimit.error());
        }
        self.queries += 1;
        let started = Instant::now();
        let attempted = self.connection.owner.query(
            "SELECT /* culprit */ CONVERT(varchar(40),SYSUTCDATETIME(),127)+'Z' AS observed_at,CONVERT(bigint,s.cpu_time) AS cpu_ms,s.logical_reads,s.reads,s.writes AS dirty_marks,s.memory_usage AS memory_pages,CAST(CASE WHEN r.session_id IS NULL THEN 0 ELSE 1 END AS bit) AS active,r.wait_type,r.blocking_session_id,CONVERT(bigint,g.granted_memory_kb) AS granted_kb,CONVERT(bigint,g.requested_memory_kb) AS requested_kb,CONVERT(bigint,g.used_memory_kb) AS used_kb FROM sys.dm_exec_sessions s LEFT JOIN sys.dm_exec_requests r ON r.session_id=s.session_id LEFT JOIN sys.dm_exec_query_memory_grants g ON g.session_id=r.session_id AND g.request_id=r.request_id WHERE s.session_id=@P1 AND CONVERT(datetime2(7),s.login_time)=CONVERT(datetime2(7),@P2,126) OPTION(MAXDOP 1)",
            vec![Parameter::Integer(self.identity.id), Parameter::Text(self.identity.login.clone())],
            2,
        ).await;
        self.elapsed_ms += started.elapsed().as_secs_f64() * 1000.0;
        let rows = match attempted {
            Ok(rows) => rows,
            Err(error) => {
                self.failed = true;
                return Err(error.into_error());
            }
        };
        let [row] = rows.as_slice() else {
            return if rows.is_empty() { Ok(None) } else { Err(CollectionStop::SourceChanged.error()) };
        };
        self.charged_bytes += value_bytes(row) as u64;
        Ok(Some(Sample {
            observed_at: canonical_time(&text(row, "observed_at")?)?,
            counters: Counters::read(row)?,
            memory_pages: optional_count(row, "memory_pages")?,
            active: boolean(row, "active")?,
            wait: optional_text(row, "wait_type")?,
            blocker: optional_signed_integer(row, "blocking_session_id")?,
            granted_kb: optional_count(row, "granted_kb")?,
            requested_kb: optional_count(row, "requested_kb")?,
            used_kb: optional_count(row, "used_kb")?,
        }))
    }
}
impl Counters {
    fn read(row: &Rec) -> culprit_core::Result<Self> {
        Ok(Self {
            cpu_ms: count(row, "cpu_ms")?,
            logical_reads: count(row, "logical_reads")?,
            reads: count(row, "reads")?,
            dirty_marks: count(row, "dirty_marks")?,
        })
    }
    fn delta(&self, after: &Self) -> culprit_core::Result<Value> {
        let delta =
            |before, after: u64| after.checked_sub(before).map(|value| value.to_string()).ok_or_else(invalid_data);
        Ok(json!({
            "cpu_ms":delta(self.cpu_ms,after.cpu_ms)?,
            "logical_reads":delta(self.logical_reads,after.logical_reads)?,
            "reads":delta(self.reads,after.reads)?,
            "dirty_marks":delta(self.dirty_marks,after.dirty_marks)?,
            "cpu_resolution":"native integer milliseconds; zero is not a zero-cost claim",
        }))
    }
}

fn qualified_delta(
    before: &Counters,
    after: &Counters,
    expected_generation: u64,
    actual_generation: u64,
) -> culprit_core::Result<Value> {
    if expected_generation != actual_generation {
        return Err(CollectionStop::SourceChanged.error());
    }
    before.delta(after)
}
impl Phase {
    fn observe(&mut self, sample: &Sample) {
        self.active_samples += u32::from(sample.active);
        if let Some(wait) = &sample.wait {
            self.waits.insert(wait.clone());
        }
        self.blocking_samples += u32::from(sample.blocker.is_some_and(|id| id > 0));
        retain_maximum(&mut self.maximum_sampled_granted_kb, sample.granted_kb);
        retain_maximum(&mut self.maximum_sampled_requested_kb, sample.requested_kb);
        retain_maximum(&mut self.maximum_sampled_used_kb, sample.used_kb);
    }
}
fn count(row: &Rec, key: &str) -> culprit_core::Result<u64> {
    optional_counter(row, key)?.ok_or_else(invalid_data)?.parse().map_err(|_| invalid_data())
}
fn parameter_values(parameters: &[Parameter]) -> Value {
    Value::Array(
        parameters
            .iter()
            .map(|value| match value {
                Parameter::Integer(value) => json!({"integer":value.to_string()}),
                Parameter::Text(value) => json!({"text":value}),
            })
            .collect(),
    )
}

struct PreparedSource {
    identity: SessionIdentity,
    context: NativeReadContext,
    authority: local_source::Context,
    report: Value,
}
async fn prepare_owned<R: NativeRead>(owner: &mut R, host: &str, port: u16) -> culprit_core::Result<PreparedSource> {
    match prepare(owner, host, port).await {
        Ok(prepared) => Ok(prepared),
        Err(error) => {
            let shutdown = owner.close().await;
            eprintln!("cost preparation failed: code={:?},shutdown={shutdown:?}", error.code);
            Err(if shutdown == SourceShutdown::Unconfirmed {
                CollectionStop::CancellationUnconfirmed.error()
            } else {
                error
            })
        }
    }
}
async fn prepare<R: NativeRead>(owner: &mut R, host: &str, port: u16) -> culprit_core::Result<PreparedSource> {
    if host.trim().is_empty() || host.contains('\0') || port == 0 {
        return Err(invalid_data());
    }
    let started = Instant::now();
    let identity=owner.query("SELECT /* culprit */ CONVERT(bigint,@@SPID) AS session_id,CONVERT(varchar(33),CONVERT(datetime2(7),login_time),126) AS login_time FROM sys.dm_exec_sessions WHERE session_id=@@SPID",vec![],1).await.map_err(ReadError::into_error)?;
    let [identity] = identity.as_slice() else {
        return Err(invalid_data());
    };
    let identity = SessionIdentity {
        id: integer(identity, "session_id")?,
        login: crate::sql_time::native_time(&text(identity, "login_time")?)?,
    };
    if !(1..=i16::MAX as i64).contains(&identity.id) || identity.login.starts_with("1900-01-01") {
        return Err(invalid_data());
    }
    let (authority, metadata) =
        local_source::read_initial(owner, host, port, IdentityDetail::Endpoint, local_source::Detail::Authority)
            .await?;
    let partition = authority.source_partition()?;
    if !authority.is_qualified() || !partition.is_qualified() {
        return Err(invalid_data());
    }
    let context = NativeReadContext {
        target: authority.activity.database.clone().into(),
        partition,
        observed_from: authority.activity.at.clone(),
        observed_until: authority.activity.at.clone(),
        database_writable: authority.activity.writable,
    };
    let preparation = json!({
        "client_elapsed_ms":started.elapsed().as_secs_f64()*1000.0,
        "queries":owner.queries(),
        "read_metrics":owner.read_metrics(),
        "scope":context,
        "session_id":identity.id,
        "login_time":identity.login,
        "native_server_version":metadata.string("server_version"),
        "native_major":authority.activity.major,
        "native_edition":authority.activity.edition,
        "temperature":"prepared-connection; cache temperature uncontrolled",
        "native_setup_cpu":"not isolated from preparation identity reads",
    });
    Ok(PreparedSource { identity, context, authority, report: preparation })
}

fn retain_maximum(retained: &mut Option<u64>, observed: Option<u64>) {
    if let Some(observed) = observed {
        *retained = Some(retained.unwrap_or(0).max(observed));
    }
}
fn optional_count(row: &Rec, key: &str) -> culprit_core::Result<Option<u64>> {
    optional_counter(row, key)?.map(|value| value.parse().map_err(|_| invalid_data())).transpose()
}
#[cfg(test)]
mod tests;

pub(crate) fn shutdown_state(shutdown: SourceShutdown) -> &'static str {
    match shutdown {
        SourceShutdown::Quiescent => "quiescent",
        SourceShutdown::Unconfirmed => "unconfirmed",
    }
}
