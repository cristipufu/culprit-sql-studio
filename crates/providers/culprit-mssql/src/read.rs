#[cfg(test)]
mod native_tests;
mod output;
use output::Output;
pub(crate) use output::ReadBatch;
use std::{sync::Arc, time::Duration};

use culprit_core::{
    Error, ErrorCode, ErrorDetail, Result,
    telemetry::{CollectionBudget, CollectionStop},
};
use futures_util::TryStreamExt;
use tiberius::{Query, QueryItem};
use tokio::time::Instant;

use crate::{
    conn::{MsClient, Pool, map_error},
    value::{Rec, Val},
};

pub(crate) enum Parameter {
    Integer(i64),
    Text(String),
}

#[derive(Debug)]
pub(crate) enum ReadError {
    Source(Error),
    Stopped(CollectionStop),
}

impl From<Error> for ReadError {
    fn from(error: Error) -> Self {
        Self::Source(error)
    }
}

impl ReadError {
    pub(crate) fn into_error(self) -> Error {
        match self {
            Self::Source(error) => error,
            Self::Stopped(reason) => reason.error(),
        }
    }
}

pub(crate) type ReadResult<T> = std::result::Result<T, ReadError>;
type OwnedRead = tokio::task::JoinHandle<ReadResult<(MsClient, Vec<Rec>)>>;

/// Safety, ownership and whole-read budget failures forbid follow-up SQL. A completed source-specific error
/// remains an independent coverage gap. RowLimit can be a per-source overflow witness; the enclosing owner must
/// separately stop when its total snapshot row budget is exhausted.
pub(crate) fn stops_remaining_reads(error: &Error) -> bool {
    matches!(error.code, ErrorCode::Timeout | ErrorCode::ConnectionFailed | ErrorCode::AuthFailed)
        || matches!(
            error.detail,
            Some(ErrorDetail::CollectionStopped(
                CollectionStop::Cancelled
                    | CollectionStop::CancellationUnconfirmed
                    | CollectionStop::TimedOut
                    | CollectionStop::QueryLimit
                    | CollectionStop::ByteLimit
                    | CollectionStop::PageLimit
                    | CollectionStop::Pressure
                    | CollectionStop::PressureUnavailable
                    | CollectionStop::SourceChanged
            ))
        )
}

struct ReadQuery {
    sql: String,
    parameters: Vec<Parameter>,
    row_limit: usize,
    byte_limit: usize,
    output: Output,
}

#[async_trait::async_trait]
pub(crate) trait NativeRead: Send {
    async fn query(&mut self, sql: &str, parameters: Vec<Parameter>, row_limit: usize) -> ReadResult<Vec<Rec>>;
    fn queries(&self) -> u32;
    fn read_metrics(&self) -> Option<culprit_core::telemetry::ReadWorkMetrics> {
        None
    }
    async fn close(&mut self) -> culprit_core::telemetry::SourceShutdown;
}

#[async_trait::async_trait]
pub(crate) trait NativeStreamRead: NativeRead {
    async fn begin_stream(&mut self, sql: &str, parameters: Vec<Parameter>, row_limit: usize) -> ReadResult<()>;
    async fn next_batch(&mut self) -> ReadResult<ReadBatch>;
}

const APPLICATION_ROWS: usize = 10_000;
const APPLICATION_PAGE_BYTES: u32 = 8 * 1024 * 1024;

pub(crate) async fn rows<R: NativeRead>(reader: &mut R, sql: &str) -> Result<Vec<Rec>> {
    let rows = reader.query(sql, vec![], APPLICATION_ROWS + 1).await.map_err(ReadError::into_error)?;
    if rows.len() > APPLICATION_ROWS {
        return Err(CollectionStop::RowLimit.error());
    }
    Ok(rows)
}
pub(crate) async fn one<R: NativeRead>(reader: &mut R, sql: &str) -> Result<Rec> {
    let mut rows = reader.query(sql, vec![], 2).await.map_err(ReadError::into_error)?;
    if rows.len() != 1 {
        return Err(invalid_data());
    }
    Ok(rows.remove(0))
}

pub(crate) struct ReadSession {
    pool: Arc<Pool>,
    client: Option<MsClient>,
    budget: CollectionBudget,
    deadline: Instant,
    queries: u32,
    active: Option<OwnedRead>,
    acquiring: bool,
    cancellation_confirmed: bool,
    metrics: crate::read_metrics::Metrics,
    streaming: Option<StreamingRead>,
}
struct StreamingRead {
    _owner: tokio::sync::oneshot::Sender<()>,
    receiver: tokio::sync::mpsc::Receiver<ReadBatch>,
}

impl ReadSession {
    pub(crate) fn application(pool: Arc<Pool>) -> Self {
        Self::new(pool, Self::application_budget())
    }
    pub(crate) fn application_budget() -> CollectionBudget {
        CollectionBudget { page_bytes: APPLICATION_PAGE_BYTES, ..Default::default() }
    }
    pub(crate) fn new(pool: Arc<Pool>, budget: CollectionBudget) -> Self {
        let deadline = Instant::now() + Duration::from_millis(budget.total_timeout_ms.into());
        Self {
            pool,
            client: None,
            budget,
            deadline,
            queries: 0,
            active: None,
            acquiring: false,
            cancellation_confirmed: true,
            metrics: crate::read_metrics::Metrics::new(culprit_core::Engine::Mssql),
            streaming: None,
        }
    }

    pub(crate) fn read_metrics(&self) -> Option<culprit_core::telemetry::ReadWorkMetrics> {
        self.metrics.snapshot()
    }
    pub(crate) fn queries(&self) -> u32 {
        self.queries
    }

    pub(crate) async fn query(
        &mut self,
        sql: &str,
        parameters: Vec<Parameter>,
        row_limit: usize,
    ) -> std::result::Result<Vec<Rec>, ReadError> {
        let owner = self.start(sql, parameters, row_limit, self.budget.page_bytes as usize, Output::buffered()).await?;
        let result = self.finish_active().await;
        drop(owner);
        result
    }
    pub(crate) async fn begin_stream(
        &mut self,
        sql: &str,
        parameters: Vec<Parameter>,
        row_limit: usize,
    ) -> ReadResult<()> {
        if self.streaming.is_some() {
            return Err(ReadError::Stopped(CollectionStop::CancellationUnconfirmed));
        }
        let (output, receiver) = Output::streaming(self.budget.page_rows as usize, self.budget.page_bytes as usize);
        let owner = self.start(sql, parameters, row_limit, self.budget.total_bytes as usize, output).await?;
        self.streaming = Some(StreamingRead { _owner: owner, receiver });
        Ok(())
    }
    pub(crate) async fn next_batch(&mut self) -> ReadResult<ReadBatch> {
        let batch =
            self.streaming.as_mut().ok_or(ReadError::Stopped(CollectionStop::InvalidPage))?.receiver.recv().await;
        match batch {
            Some(batch) if !batch.complete => Ok(batch),
            Some(batch) => {
                let result = self.finish_active().await;
                self.streaming.take();
                result?;
                Ok(batch)
            }
            None => {
                let result = self.finish_active().await;
                self.streaming.take();
                result?;
                Err(ReadError::Stopped(CollectionStop::InvalidPage))
            }
        }
    }
    async fn start(
        &mut self,
        sql: &str,
        parameters: Vec<Parameter>,
        row_limit: usize,
        byte_limit: usize,
        output: Output,
    ) -> ReadResult<tokio::sync::oneshot::Sender<()>> {
        let timeout = self.admit()?;
        let client = match self.client.take() {
            Some(client) => client,
            None => self.acquire(timeout).await?,
        };
        let timeout = self.remaining_statement_time()?;
        let (owner, cancelled) = tokio::sync::oneshot::channel::<()>();
        // The task owns the socket until result completion or Attention acknowledgement/disconnect. Dropping the
        // caller requests cancellation instead of dropping a future while leaving SQL work running on the server.
        let query = ReadQuery {
            sql: crate::statement_origin::monitoring_batch(sql),
            parameters,
            row_limit,
            byte_limit,
            output,
        };
        let transport = self.pool.transport_owner();
        let template = self.metrics.begin(sql);
        self.active = Some(tokio::spawn(read_owned(
            client,
            query,
            timeout,
            cancelled,
            transport,
            self.metrics.clone(),
            template,
        )));
        Ok(owner)
    }
    async fn finish_active(&mut self) -> ReadResult<Vec<Rec>> {
        let result = self.active.as_mut().expect("the session owns its task").await;
        self.active.take();
        if matches!(&result, Err(_) | Ok(Err(ReadError::Stopped(CollectionStop::CancellationUnconfirmed)))) {
            self.cancellation_confirmed = false;
            self.pool.mark_uncertain();
        }
        let result = result.map_err(|_| ReadError::Stopped(CollectionStop::CancellationUnconfirmed))?;
        let (client, rows) = result?;
        self.client = Some(client);
        Ok(rows)
    }

    fn admit(&mut self) -> ReadResult<Duration> {
        self.pool.ensure_available().map_err(|_| ReadError::Stopped(CollectionStop::CancellationUnconfirmed))?;
        if self.acquiring {
            self.cancellation_confirmed = false;
        }
        if self.active.is_some() {
            return Err(ReadError::Stopped(CollectionStop::CancellationUnconfirmed));
        }
        if !self.cancellation_confirmed {
            return Err(ReadError::Stopped(CollectionStop::CancellationUnconfirmed));
        }
        if self.queries >= self.budget.max_queries {
            return Err(ReadError::Stopped(CollectionStop::QueryLimit));
        }
        let remaining = self.remaining_statement_time()?;
        self.queries += 1;
        Ok(remaining)
    }
    async fn acquire(&mut self, timeout: Duration) -> ReadResult<MsClient> {
        // A dropped acquire may have reached login/session setup. Without a returned driver there is no
        // acknowledgement channel, so close/admission must retain the uncertainty instead of starting replacement work.
        self.acquiring = true;
        let result = tokio::time::timeout(timeout, self.pool.get()).await;
        self.acquiring = false;
        match result {
            Ok(result) => result.map_err(ReadError::Source),
            Err(_) => {
                self.cancellation_confirmed = false;
                self.pool.mark_uncertain();
                Err(ReadError::Stopped(CollectionStop::TimedOut))
            }
        }
    }

    fn remaining_statement_time(&self) -> ReadResult<Duration> {
        let timeout = self
            .deadline
            .saturating_duration_since(Instant::now())
            .min(Duration::from_millis(self.budget.statement_timeout_ms.into()));
        if timeout.is_zero() { Err(ReadError::Stopped(CollectionStop::TimedOut)) } else { Ok(timeout) }
    }

    pub(crate) async fn close(&mut self) -> culprit_core::telemetry::SourceShutdown {
        // Release both the cancellation sender and any blocked channel delivery before joining the native owner.
        self.streaming.take();
        if self.acquiring {
            self.cancellation_confirmed = false;
        }
        if self.active.is_some() {
            // Keep the handle in this owner across await: cancelling close must not admit replacement SQL.
            // finish_active records shutdown uncertainty; the return value is represented by SourceShutdown below.
            let _ = self.finish_active().await;
        }
        if let Some(client) = self.client.take()
            && self.cancellation_confirmed
        {
            self.pool.put(client);
        }
        if !self.cancellation_confirmed {
            self.pool.mark_uncertain();
        }
        self.pool.source_shutdown()
    }

    pub(crate) async fn finish<T>(&mut self, result: Result<T>) -> Result<T> {
        if self.close().await == culprit_core::telemetry::SourceShutdown::Unconfirmed {
            return Err(CollectionStop::CancellationUnconfirmed.error());
        }
        result
    }
}

impl Drop for ReadSession {
    fn drop(&mut self) {
        if self.acquiring || self.active.is_some() || !self.cancellation_confirmed {
            self.pool.mark_uncertain();
        }
    }
}

#[async_trait::async_trait]
impl NativeRead for ReadSession {
    async fn query(&mut self, sql: &str, parameters: Vec<Parameter>, row_limit: usize) -> ReadResult<Vec<Rec>> {
        ReadSession::query(self, sql, parameters, row_limit).await
    }
    fn queries(&self) -> u32 {
        self.queries()
    }
    fn read_metrics(&self) -> Option<culprit_core::telemetry::ReadWorkMetrics> {
        self.read_metrics()
    }
    async fn close(&mut self) -> culprit_core::telemetry::SourceShutdown {
        ReadSession::close(self).await
    }
}

#[async_trait::async_trait]
impl NativeStreamRead for ReadSession {
    async fn begin_stream(&mut self, sql: &str, parameters: Vec<Parameter>, row_limit: usize) -> ReadResult<()> {
        ReadSession::begin_stream(self, sql, parameters, row_limit).await
    }
    async fn next_batch(&mut self) -> ReadResult<ReadBatch> {
        ReadSession::next_batch(self).await
    }
}

const CANCELLATION_DRAIN_GRACE: Duration = Duration::from_millis(250);
const CANCELLATION_ACK_TIMEOUT: Duration = Duration::from_secs(1);

pub(crate) enum ReadOutcome {
    Completed(Vec<Rec>),
    QuiescentStop(CollectionStop),
    NeedsAttention(ReadError),
}

async fn finish_or_interrupt(
    work: impl std::future::Future<Output = ReadResult<Vec<Rec>>>,
    timeout: Duration,
    cancelled: tokio::sync::oneshot::Receiver<()>,
    retention: culprit_read::RetentionControl,
) -> ReadOutcome {
    use std::sync::atomic::{AtomicBool, Ordering};
    let started = AtomicBool::new(false);
    tokio::pin!(work);
    let tracked = std::future::poll_fn(|context| {
        started.store(true, Ordering::Relaxed);
        work.as_mut().poll(context)
    });
    tokio::pin!(tracked);
    let reason = tokio::select! {
        biased;
        _ = cancelled => CollectionStop::Cancelled,
        _ = tokio::time::sleep(timeout) => CollectionStop::TimedOut,
        result = &mut tracked => return match result {
            Ok(rows) => ReadOutcome::Completed(rows),
            Err(error) => ReadOutcome::NeedsAttention(error),
        },
    };
    if !started.load(Ordering::Relaxed) {
        return ReadOutcome::QuiescentStop(reason);
    }
    retention.discard();
    // Keep the existing decoder alive briefly: dropping it mid-token can desynchronize the driver and prevent
    // Attention acknowledgement. No new request starts, and cancelled rows are never accepted as a page.
    match tokio::time::timeout(CANCELLATION_DRAIN_GRACE, &mut tracked).await {
        Ok(Ok(_)) => ReadOutcome::QuiescentStop(reason),
        _ => ReadOutcome::NeedsAttention(ReadError::Stopped(reason)),
    }
}

async fn read_owned(
    mut client: MsClient,
    query: ReadQuery,
    timeout: Duration,
    cancelled: tokio::sync::oneshot::Receiver<()>,
    transport: Option<Arc<dyn culprit_core::ConnectionTransport>>,
    metrics: crate::read_metrics::Metrics,
    template: Option<String>,
) -> ReadResult<(MsClient, Vec<Rec>)> {
    // An SSH route must outlive the caller until Attention acknowledgement or disconnect.
    let _transport = transport;
    let started = std::time::Instant::now();
    let progress = Arc::new(crate::read_metrics::Progress::default());
    let retention = query.output.retention_control();
    let outcome = finish_or_interrupt(read(&mut client, query, progress.clone()), timeout, cancelled, retention).await;
    let mut accounting = crate::read_metrics::requested(&outcome);
    let result = match outcome {
        ReadOutcome::Completed(rows) => Ok((client, rows)),
        ReadOutcome::QuiescentStop(reason) => {
            drop(client);
            Err(ReadError::Stopped(reason))
        }
        ReadOutcome::NeedsAttention(error) if progress.drained.load(std::sync::atomic::Ordering::Relaxed) => {
            drop(client);
            Err(error)
        }
        ReadOutcome::NeedsAttention(error) => {
            // Tiberius 0.13.0 can stop at the cancelled response's message boundary before reading a separate
            // DONE_ATTN acknowledgement: https://github.com/tiberius-rs/tiberius/pull/460 (unreleased).
            // Update to a published version containing that fix and rerun the native cancellation regressions.
            // Until then, a missing acknowledgement remains Unconfirmed and blocks replacement work; neither
            // this known driver error nor a closed socket proves native shutdown. Do not suppress it or retry SQL.
            let acknowledgement = tokio::time::timeout(CANCELLATION_ACK_TIMEOUT, client.cancel_query()).await;
            #[cfg(test)]
            match &acknowledgement {
                Ok(Err(error)) => eprintln!("Native cancellation acknowledgement failed: {error}"),
                Err(_) => eprintln!("Native cancellation acknowledgement exceeded its bounded deadline"),
                _ => {}
            }
            let acknowledged = matches!(acknowledgement, Ok(Ok(())));
            drop(client);
            Err(if acknowledged { error } else { ReadError::Stopped(CollectionStop::CancellationUnconfirmed) })
        }
    };
    accounting.failed = result.is_err();
    accounting.unconfirmed = matches!(&result, Err(ReadError::Stopped(CollectionStop::CancellationUnconfirmed)));
    metrics.finish(template.as_deref(), started.elapsed(), &progress, &accounting);
    result
}

async fn read(
    client: &mut MsClient,
    request: ReadQuery,
    progress: Arc<crate::read_metrics::Progress>,
) -> ReadResult<Vec<Rec>> {
    let ReadQuery { sql, parameters, row_limit, byte_limit, mut output } = request;
    let mut query = Query::new(&sql);
    for parameter in &parameters {
        match parameter {
            Parameter::Integer(value) => query.bind(*value),
            Parameter::Text(value) => query.bind(value.as_str()),
        }
    }
    let mut stream = query.query(client).await.map_err(map_error)?;
    let mut budget = culprit_read::ReadBudget::new(row_limit, byte_limit);
    let mut sets = 0;
    while let Some(item) = stream.try_next().await.map_err(map_error)? {
        match item {
            QueryItem::Metadata(_) => {
                sets += 1;
                if sets > 1 {
                    return Err(ReadError::Stopped(CollectionStop::InvalidPage));
                }
            }
            QueryItem::Row(row) => {
                // Receiver release is paired with cancellation. Continue consuming the existing response during
                // its bounded drain grace, without retaining rows or starting replacement work.
                if output.discarding() {
                    continue;
                }
                budget.row_started().map_err(|error| ReadError::Stopped(CollectionStop::from_error(&error)))?;
                let row = Rec::from_row(&row);
                progress.rows.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let row_bytes = output::value_bytes(&row);
                progress.bytes.fetch_add(row_bytes as u64, std::sync::atomic::Ordering::Relaxed);
                budget
                    .charge_values(row_bytes)
                    .map_err(|error| ReadError::Stopped(CollectionStop::from_error(&error)))?;
                output.push(row, row_bytes).await?;
            }
        }
    }
    // Final delivery may still face consumer backpressure, but the database statement is now fully drained.
    progress.drained.store(true, std::sync::atomic::Ordering::Relaxed);
    Ok(output.finish().await)
}

pub(crate) fn invalid_data() -> Error {
    Error::new(culprit_core::ErrorCode::QueryFailed, culprit_core::t!("collection.invalid_source"))
        .with_detail(culprit_core::ErrorDetail::CollectionStopped(CollectionStop::InvalidPage))
}
pub(crate) fn integer(row: &Rec, name: &str) -> culprit_core::Result<i64> {
    signed_integer(row, name).and_then(|value| if value >= 0 { Ok(value) } else { Err(invalid_data()) })
}
pub(crate) fn signed_integer(row: &Rec, name: &str) -> culprit_core::Result<i64> {
    match row.columns().find(|(column, _)| *column == name).map(|(_, value)| value) {
        Some(Val::Int(value)) => Ok(*value),
        _ => Err(invalid_data()),
    }
}
pub(crate) fn optional_signed_integer(row: &Rec, name: &str) -> Result<Option<i64>> {
    match row.columns().find(|(column, _)| *column == name).map(|(_, value)| value) {
        Some(Val::Null) => Ok(None),
        Some(Val::Int(value)) => Ok(Some(*value)),
        _ => Err(invalid_data()),
    }
}
pub(crate) fn optional_integer(row: &Rec, name: &str) -> Result<Option<i64>> {
    match optional_signed_integer(row, name)? {
        Some(value) if value < 0 => Err(invalid_data()),
        value => Ok(value),
    }
}
pub(crate) fn optional_counter(row: &Rec, name: &str) -> Result<Option<String>> {
    Ok(optional_integer(row, name)?.map(|value| value.to_string()))
}
pub(crate) fn optional_boolean(row: &Rec, name: &str) -> Result<Option<bool>> {
    match row.columns().find(|(column, _)| *column == name).map(|(_, value)| value) {
        Some(Val::Null) => Ok(None),
        Some(Val::Bool(value)) => Ok(Some(*value)),
        Some(Val::Int(0)) => Ok(Some(false)),
        Some(Val::Int(1)) => Ok(Some(true)),
        _ => Err(invalid_data()),
    }
}
pub(crate) fn boolean(row: &Rec, name: &str) -> Result<bool> {
    optional_boolean(row, name)?.ok_or_else(invalid_data)
}
pub(crate) fn text(row: &Rec, name: &str) -> culprit_core::Result<String> {
    row.str(name).filter(|value| !value.is_empty()).ok_or_else(invalid_data)
}
pub(crate) fn optional_text(row: &Rec, name: &str) -> Result<Option<String>> {
    match row.columns().find(|(key, _)| *key == name).map(|(_, value)| value) {
        Some(Val::Null) => Ok(None),
        Some(Val::Text(value)) => Ok(Some(value.clone())),
        _ => Err(invalid_data()),
    }
}
pub(crate) fn require_primary(writable: Option<bool>) -> Result<()> {
    if writable == Some(true) {
        Ok(())
    } else {
        Err(Error::new(culprit_core::ErrorCode::Unsupported, culprit_core::t!("collection.azure_sql_primary_only")))
    }
}

pub(crate) fn database_writable(row: &Rec) -> Result<Option<bool>> {
    match row.str("updateability").as_deref() {
        Some("READ_WRITE") => Ok(Some(true)),
        Some("READ_ONLY") => Ok(Some(false)),
        None => Ok(None),
        _ => Err(invalid_data()),
    }
}

pub(crate) fn required_nonnegative_number(row: &Rec, name: &str) -> Result<f64> {
    optional_nonnegative_number(row, name)?.ok_or_else(invalid_data)
}
pub(crate) fn optional_nonnegative_number(row: &Rec, name: &str) -> Result<Option<f64>> {
    use crate::value::Val;
    match row.columns().find(|(key, _)| *key == name).map(|(_, value)| value) {
        Some(Val::Null) => Ok(None),
        Some(Val::Int(value)) if *value >= 0 => Ok(Some(*value as f64)),
        Some(Val::Float(value)) if value.is_finite() && *value >= 0.0 => Ok(Some(*value)),
        _ => Err(invalid_data()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct StalledLogin {
        pool: Arc<Pool>,
        accepted: tokio::sync::oneshot::Receiver<()>,
        release: tokio::sync::oneshot::Sender<()>,
        server: tokio::task::JoinHandle<()>,
    }
    async fn stalled_login() -> StalledLogin {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let (accepted, entered) = tokio::sync::oneshot::channel();
        let (release, released) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let _ = accepted.send(());
            let _ = released.await;
            drop(socket);
        });
        let mut config = tiberius::Config::new();
        config.host("127.0.0.1");
        config.port(port);
        config.authentication(tiberius::AuthMethod::sql_server("fixture", "fixture-password"));
        StalledLogin { pool: Arc::new(Pool::new(config, None)), accepted: entered, release, server }
    }
    #[tokio::test]
    async fn timing_out_during_login_cannot_report_quiescence_or_admit_replacement_work() {
        let fixture = stalled_login().await;
        let budget = CollectionBudget { statement_timeout_ms: 20, total_timeout_ms: 1000, ..Default::default() };
        let mut session = ReadSession::new(fixture.pool, budget);
        assert!(matches!(
            session.query("SELECT 1", vec![], 1).await,
            Err(ReadError::Stopped(CollectionStop::TimedOut))
        ));
        assert!(matches!(session.admit(), Err(ReadError::Stopped(CollectionStop::CancellationUnconfirmed))));
        assert_eq!(session.close().await, culprit_core::telemetry::SourceShutdown::Unconfirmed);
        let _ = fixture.release.send(());
        fixture.server.await.unwrap();
    }
    #[tokio::test]
    async fn dropping_a_read_during_login_keeps_uncertainty_visible_to_close() {
        let mut fixture = stalled_login().await;
        let mut session = ReadSession::new(fixture.pool, CollectionBudget::default());
        {
            let query = session.query("SELECT 1", vec![], 1);
            tokio::pin!(query);
            tokio::select! {
                _=tokio::time::sleep(Duration::from_secs(2))=>panic!("loopback login was not reached"),
                _=&mut fixture.accepted=>{},
                _=&mut query=>panic!("the stalled login cannot finish a query"),
            }
        }
        assert_eq!(session.close().await, culprit_core::telemetry::SourceShutdown::Unconfirmed);
        assert!(matches!(session.admit(), Err(ReadError::Stopped(CollectionStop::CancellationUnconfirmed))));
        let _ = fixture.release.send(());
        fixture.server.await.unwrap();
    }
    #[tokio::test]
    async fn a_refused_dial_is_not_confused_with_lost_native_login_or_setup_work() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let mut config = tiberius::Config::new();
        config.host("127.0.0.1");
        config.port(port);
        let pool = Arc::new(Pool::new(config, None));
        let mut reader = ReadSession::application(pool.clone());
        assert!(reader.query("SELECT 1", vec![], 1).await.is_err());
        assert_eq!(reader.close().await, culprit_core::telemetry::SourceShutdown::Quiescent);
        assert_eq!(pool.source_shutdown(), culprit_core::telemetry::SourceShutdown::Quiescent);
    }
    fn session() -> ReadSession {
        ReadSession::new(
            Arc::new(Pool::new(tiberius::Config::new(), None)),
            CollectionBudget { max_queries: 1, ..CollectionBudget::default() },
        )
    }
    #[tokio::test]
    async fn interrupted_close_keeps_the_owned_task_until_its_shutdown_outcome_is_recorded() {
        for (stop, expected) in [
            (CollectionStop::Cancelled, culprit_core::telemetry::SourceShutdown::Quiescent),
            (CollectionStop::CancellationUnconfirmed, culprit_core::telemetry::SourceShutdown::Unconfirmed),
        ] {
            let mut session = session();
            let (release, waiting) = tokio::sync::oneshot::channel::<()>();
            session.active = Some(tokio::spawn(async move {
                waiting.await.unwrap();
                Err(ReadError::Stopped(stop))
            }));
            let mut closing = Box::pin(session.close());
            assert!(matches!(futures_util::poll!(closing.as_mut()), std::task::Poll::Pending));
            drop(closing);
            assert!(session.active.is_some());
            assert!(matches!(session.admit(), Err(ReadError::Stopped(CollectionStop::CancellationUnconfirmed))));
            assert_eq!(session.queries(), 0);
            release.send(()).unwrap();
            assert_eq!(session.close().await, expected);
            assert!(session.active.is_none());
            if expected == culprit_core::telemetry::SourceShutdown::Unconfirmed {
                assert!(matches!(session.admit(), Err(ReadError::Stopped(CollectionStop::CancellationUnconfirmed))));
            }
        }
    }

    #[tokio::test]
    async fn one_reader_losing_shutdown_proof_blocks_every_other_reader_in_the_provider() {
        let pool = Arc::new(Pool::new(tiberius::Config::new(), None));
        let mut owner = ReadSession::application(pool.clone());
        let mut already_open = ReadSession::application(pool.clone());
        owner.acquiring = true;
        drop(owner);
        for reader in [&mut already_open, &mut ReadSession::application(pool.clone())] {
            assert!(matches!(
                reader.query("SELECT 1", vec![], 1).await,
                Err(ReadError::Stopped(CollectionStop::CancellationUnconfirmed))
            ));
            assert_eq!(
                reader.queries(),
                0,
                "shared uncertainty blocks before native admission or connection acquisition"
            );
            assert_eq!(reader.close().await, culprit_core::telemetry::SourceShutdown::Unconfirmed);
        }
        assert!(pool.get().await.is_err());
        assert_eq!(pool.source_shutdown(), culprit_core::telemetry::SourceShutdown::Unconfirmed);
    }

    #[test]
    fn exhausted_budget_or_uncertain_work_never_admits_another_native_read() {
        let mut session = session();
        assert!(session.admit().is_ok());
        assert!(matches!(session.admit(), Err(ReadError::Stopped(CollectionStop::QueryLimit))));
        session.cancellation_confirmed = false;
        assert!(matches!(session.admit(), Err(ReadError::Stopped(CollectionStop::CancellationUnconfirmed))));
        assert_eq!(session.queries(), 1);
    }
    #[test]
    fn an_expired_deadline_is_rejected_before_connection_acquisition() {
        let mut session = session();
        session.deadline = Instant::now();
        assert!(matches!(session.admit(), Err(ReadError::Stopped(CollectionStop::TimedOut))));
        assert_eq!(session.queries(), 0);
    }
    #[tokio::test(start_paused = true)]
    async fn cancellation_before_start_never_polls_the_native_request() {
        let (owner, cancelled) = tokio::sync::oneshot::channel();
        drop(owner);
        let outcome = finish_or_interrupt(
            async { panic!("a cancelled request must never start") },
            Duration::from_secs(5),
            cancelled,
            culprit_read::RetentionControl::default(),
        )
        .await;
        assert!(matches!(outcome, ReadOutcome::QuiescentStop(CollectionStop::Cancelled)));
    }

    #[tokio::test(start_paused = true)]
    async fn an_inflight_read_can_finish_without_dropping_its_decoder_on_cancellation() {
        let (owner, cancelled) = tokio::sync::oneshot::channel();
        let completed = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let done = completed.clone();
        let work = async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            done.store(true, std::sync::atomic::Ordering::SeqCst);
            Ok(vec![])
        };
        tokio::spawn(async {
            tokio::time::sleep(Duration::from_millis(10)).await;
            drop(owner);
        });
        let outcome =
            finish_or_interrupt(work, Duration::from_secs(5), cancelled, culprit_read::RetentionControl::default())
                .await;
        assert!(completed.load(std::sync::atomic::Ordering::SeqCst));
        assert!(matches!(outcome, ReadOutcome::QuiescentStop(CollectionStop::Cancelled)));
    }

    #[tokio::test(start_paused = true)]
    async fn a_stalled_read_only_gets_a_bounded_drain_grace() {
        let (owner, cancelled) = tokio::sync::oneshot::channel();
        tokio::spawn(async {
            tokio::time::sleep(Duration::from_millis(10)).await;
            drop(owner);
        });
        let started = Instant::now();
        let outcome = finish_or_interrupt(
            std::future::pending(),
            Duration::from_secs(5),
            cancelled,
            culprit_read::RetentionControl::default(),
        )
        .await;
        assert!(matches!(outcome, ReadOutcome::NeedsAttention(ReadError::Stopped(CollectionStop::Cancelled))));
        assert_eq!(started.elapsed(), Duration::from_millis(10) + CANCELLATION_DRAIN_GRACE);
    }
}
