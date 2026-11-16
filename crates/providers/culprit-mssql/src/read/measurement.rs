//! Owned-fixture instrumentation around the real reader. Observer and preparation work are separate from source work.
use super::*;
use crate::{activity::IdentityDetail, conn::SessionKind, local_source, test_support::config};
use culprit_core::telemetry::*;
use serde_json::{Value, json};
use std::sync::atomic::{AtomicBool, Ordering};

const OBSERVER_TIMEOUT: Duration = Duration::from_secs(3);
const SAMPLE_INTERVAL: Duration = Duration::from_millis(10);
const MAX_PHASES: usize = 32;
const MAX_OBSERVER_QUERIES: u32 = 512;
mod lifecycle;
pub(crate) use lifecycle::{assert_no_followup, lifecycle_cases};

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
#[derive(Clone)]
struct Counters {
    cpu: u64,
    logical: u64,
    physical: u64,
    writes: u64,
}
struct Sample {
    counters: Counters,
    active: bool,
    wait: Option<String>,
    blocker: Option<i64>,
    granted_kb: Option<u64>,
    requested_kb: Option<u64>,
    used_kb: Option<u64>,
}
struct Observer {
    client: MsClient,
    identity: SessionIdentity,
    queries: u32,
    elapsed_ms: f64,
    charged_bytes: u64,
    failed: bool,
    setup_elapsed_ms: f64,
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
}

pub(crate) struct MeasuredRead {
    owner: ReadSession,
    observer: Observer,
    generation: u64,
    context: NativeReadContext,
    preparation: Value,
    phase: Option<Phase>,
    phases: Vec<Value>,
    active_seen: Arc<AtomicBool>,
    template_watch: Option<TemplateObservation>,
    errors: Vec<String>,
}

impl MeasuredRead {
    pub(crate) async fn open(
        pool: Arc<Pool>,
        port: u16,
        password: &str,
        database: &str,
        budget: CollectionBudget,
    ) -> culprit_core::Result<Self> {
        let mut owner = ReadSession::new(
            pool,
            CollectionBudget { max_queries: 16, total_timeout_ms: 10_000, ..Default::default() },
        );
        let prepared = prepare(&mut owner, port).await;
        let (identity, context, preparation) = match prepared {
            Ok(prepared) => prepared,
            Err(error) => {
                let shutdown = owner.close().await;
                eprintln!("cost preparation failed: code={:?},shutdown={shutdown:?}", error.code);
                return Err(error);
            }
        };
        let observer_pool = Pool::new(config(port, database, password, "culprit-cost-observer"), None);
        let observer_started = Instant::now();
        let client = match observer_pool.connect(SessionKind::User).await {
            Ok(client) => client,
            Err(error) => {
                let shutdown = owner.close().await;
                eprintln!("cost observer acquisition failed: code={:?},source_shutdown={shutdown:?}", error.code);
                return Err(error);
            }
        };
        let observer = Observer {
            client,
            identity,
            queries: 0,
            elapsed_ms: 0.0,
            charged_bytes: 0,
            failed: false,
            setup_elapsed_ms: observer_started.elapsed().as_secs_f64() * 1000.0,
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
            preparation,
            phase: None,
            phases: vec![],
            active_seen: Arc::new(AtomicBool::new(false)),
            template_watch: None,
            errors: vec![],
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
    pub(crate) async fn is_quiet(&mut self) -> culprit_core::Result<bool> {
        self.observer.is_quiet().await
    }
    pub(crate) fn report(&self) -> Value {
        json!({
            "preparation":self.preparation,
            "source_read_metrics":self.owner.read_metrics(),
            "source_queries":self.owner.queries(),
            "phases":self.phases,
            "observer":{"setup_client_elapsed_ms":self.observer.setup_elapsed_ms,"queries":self.observer.queries,"client_elapsed_ms":self.observer.elapsed_ms,"charged_value_bytes":self.observer.charged_bytes.to_string()},
            "grant_evidence":"sampled-current-grants-only; missed samples do not establish no grant or a peak",
            "measurement_errors":self.errors,
            "measurement_complete":self.errors.is_empty() && self.phase.is_none() && self.phases.len()==self.owner.queries() as usize,
            "native_cpu_io_cost_qualified":false,
        })
    }

    async fn begin(&mut self, sql: &str, parameters: &[Parameter], row_limit: usize) -> culprit_core::Result<()> {
        if self.phase.is_some() || self.phases.len() == MAX_PHASES {
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
        });
        Ok(())
    }
    async fn end(&mut self, outcome: &str) -> culprit_core::Result<()> {
        let after = self.observer.sample().await;
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
        Ok(())
    }
}

#[async_trait::async_trait]
impl NativeRead for MeasuredRead {
    async fn query(&mut self, sql: &str, parameters: Vec<Parameter>, row_limit: usize) -> ReadResult<Vec<Rec>> {
        self.begin(sql, &parameters, row_limit).await.map_err(ReadError::Source)?;
        let mut observer_error = None;
        let result = {
            let work = self.owner.query(sql, parameters, row_limit);
            tokio::pin!(work);
            loop {
                tokio::select! {
                    result = &mut work => break result,
                    _ = tokio::time::sleep(SAMPLE_INTERVAL) => {
                        match self.observer.sample().await {
                            Ok(sample) => {
                                if sample.active { self.active_seen.store(true, Ordering::Release); }
                                if let (Some(watch), Some(phase)) = (&self.template_watch, &self.phase) {
                                    watch.observe(&phase.sql, sample.active);
                                }
                                if let Some(phase) = &mut self.phase { phase.observe(&sample); }
                            }
                            Err(error) => { observer_error = Some(error); break Err(ReadError::Stopped(CollectionStop::Cancelled)); }
                        }
                    }
                }
            }
        };
        let outcome = match &result {
            Ok(_) => "completed",
            Err(ReadError::Source(_)) => "source-error",
            Err(ReadError::Stopped(_)) => "stopped",
        };
        // If observing failed, first settle the real owner. Its native shutdown cannot be replaced by an
        // instrumentation error or by an observer later finding no request.
        if let Some(error) = &observer_error {
            self.errors.push(format!("observer:{:?}", error.code));
            let _ = self.owner.close().await;
        }
        let accounting = self.end(outcome).await;
        match result {
            Err(error) => Err(error),
            Ok(rows) => {
                if let Some(error) = observer_error {
                    return Err(ReadError::Source(error));
                }
                accounting.map_err(ReadError::Source)?;
                Ok(rows)
            }
        }
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
        let shutdown = self.owner.close().await;
        if self.phase.is_some() {
            let _ = self.end("interrupted-before-completion").await;
        }
        shutdown
    }
}

#[async_trait::async_trait]
impl NativeStreamRead for MeasuredRead {
    async fn begin_stream(&mut self, sql: &str, parameters: Vec<Parameter>, row_limit: usize) -> ReadResult<()> {
        self.begin(sql, &parameters, row_limit).await.map_err(ReadError::Source)?;
        self.owner.begin_stream(sql, parameters, row_limit).await
    }
    async fn next_batch(&mut self) -> ReadResult<ReadBatch> {
        let result = self.owner.next_batch().await;
        if result.as_ref().is_ok_and(|batch| batch.complete) {
            self.end("stream-completed").await.map_err(ReadError::Source)?;
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
        let mut query = Query::new(
            "SELECT /* culprit fixture observer */ CONVERT(bigint,s.cpu_time) AS cpu_ms,s.logical_reads,s.reads,s.writes,CAST(CASE WHEN r.session_id IS NULL THEN 0 ELSE 1 END AS bit) AS active,r.wait_type,r.blocking_session_id,CONVERT(bigint,g.granted_memory_kb) AS granted_kb,CONVERT(bigint,g.requested_memory_kb) AS requested_kb,CONVERT(bigint,g.used_memory_kb) AS used_kb FROM sys.dm_exec_sessions s LEFT JOIN sys.dm_exec_requests r ON r.session_id=s.session_id LEFT JOIN sys.dm_exec_query_memory_grants g ON g.session_id=r.session_id AND g.request_id=r.request_id WHERE s.session_id=@P1 AND CONVERT(datetime2(7),s.login_time)=CONVERT(datetime2(7),@P2,126) OPTION(MAXDOP 1)",
        );
        query.bind(self.identity.id);
        query.bind(self.identity.login.as_str());
        let attempted = tokio::time::timeout(OBSERVER_TIMEOUT, async {
            query
                .query(&mut self.client)
                .await
                .map_err(crate::conn::map_error)?
                .into_first_result()
                .await
                .map_err(crate::conn::map_error)
        })
        .await;
        self.elapsed_ms += started.elapsed().as_secs_f64() * 1000.0;
        let rows = match attempted {
            Ok(Ok(rows)) => rows,
            Ok(Err(error)) => {
                self.failed = true;
                return Err(error);
            }
            Err(_) => {
                self.failed = true;
                return Err(CollectionStop::TimedOut.error());
            }
        };
        let rows = rows.iter().map(Rec::from_row).collect::<Vec<_>>();
        let [row] = rows.as_slice() else {
            return if rows.is_empty() { Ok(None) } else { Err(CollectionStop::SourceChanged.error()) };
        };
        self.charged_bytes += value_bytes(row) as u64;
        Ok(Some(Sample {
            counters: Counters::read(row)?,
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
            cpu: count(row, "cpu_ms")?,
            logical: count(row, "logical_reads")?,
            physical: count(row, "reads")?,
            writes: count(row, "writes")?,
        })
    }
    fn delta(&self, after: &Self) -> culprit_core::Result<Value> {
        let delta =
            |before, after: u64| after.checked_sub(before).map(|value| value.to_string()).ok_or_else(invalid_data);
        Ok(json!({
            "cpu_ms":delta(self.cpu,after.cpu)?,
            "logical_reads":delta(self.logical,after.logical)?,
            "physical_reads":delta(self.physical,after.physical)?,
            "writes":delta(self.writes,after.writes)?,
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

async fn prepare(
    owner: &mut ReadSession,
    port: u16,
) -> culprit_core::Result<(SessionIdentity, NativeReadContext, Value)> {
    let started = Instant::now();
    let identity=owner.query("SELECT /* culprit fixture preparation */ CONVERT(bigint,@@SPID) AS session_id,CONVERT(varchar(33),CONVERT(datetime2(7),login_time),126) AS login_time FROM sys.dm_exec_sessions WHERE session_id=@@SPID",vec![],1).await.map_err(ReadError::into_error)?;
    let [identity] = identity.as_slice() else {
        return Err(invalid_data());
    };
    let identity = SessionIdentity { id: integer(identity, "session_id")?, login: text(identity, "login_time")? };
    let (authority, metadata) =
        local_source::read_initial(owner, "127.0.0.1", port, IdentityDetail::Endpoint, local_source::Detail::Authority)
            .await?;
    let partition = authority.source_partition()?;
    if !authority.is_qualified() || !partition.is_qualified() {
        return Err(invalid_data());
    }
    let context = NativeReadContext {
        target: authority.activity.database.clone().into(),
        partition,
        observed_from: authority.activity.at.clone(),
        observed_until: authority.activity.at,
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
    Ok((identity, context, preparation))
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
