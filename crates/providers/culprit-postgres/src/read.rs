//! One owner for bounded native reads. Caller cancellation never abandons a response mid-protocol.
mod application;
#[cfg(test)]
mod native_tests;
mod owned;
mod response;
use crate::{
    connect::{Connector, SessionLimits},
    rows::Row,
};
pub(crate) use application::ApplicationReadScope;
use culprit_core::{
    Engine, Result,
    telemetry::{CollectionBudget, CollectionStop, ReadMetricsRecorder, ReadWorkMetrics, SourceShutdown},
};
use culprit_read::{ReadBatch, RowOutput};
use response::{ReadOutput, ReadRows};
use std::{
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};
use tokio::{task::JoinHandle, time::Instant};
use tokio_postgres::Client;

pub(crate) struct CompletedRead {
    client: Option<Client>,
    result: NativeResult<ReadRows>,
    confirmed: bool,
}
/// Error metadata travels with this exact owned response, never a previous query or a localized message.
#[derive(Debug)]
pub struct NativeReadFailure {
    pub error: culprit_core::Error,
    pub sql_state: Option<tokio_postgres::error::SqlState>,
}
pub type NativeResult<T> = std::result::Result<T, NativeReadFailure>;
impl From<culprit_core::Error> for NativeReadFailure {
    fn from(error: culprit_core::Error) -> Self {
        Self { error, sql_state: None }
    }
}
impl From<tokio_postgres::Error> for NativeReadFailure {
    fn from(error: tokio_postgres::Error) -> Self {
        let sql_state = error.as_db_error().map(|error| error.code().clone());
        Self { error: crate::connect::map_error(error), sql_state }
    }
}

#[async_trait::async_trait]
pub trait QueryRead: Send {
    async fn query(&mut self, sql: &str, row_limit: usize) -> Result<Vec<Row>>;
    async fn query_with_state(&mut self, sql: &str, row_limit: usize) -> NativeResult<Vec<Row>> {
        self.query(sql, row_limit).await.map_err(NativeReadFailure::from)
    }
    async fn batch_execute(&mut self, sql: &str) -> Result<()> {
        self.query(sql, culprit_core::limits::MAX_RESULT_ROWS).await.map(|_| ())
    }
    async fn query_text_array(
        &mut self,
        _sql: &str,
        _values: Vec<String>,
        _row_limit: usize,
    ) -> Result<Vec<tokio_postgres::Row>> {
        Err(culprit_core::Error::new(culprit_core::ErrorCode::Unsupported, culprit_core::t!("collection.unsupported")))
    }
}
/// Borrowed interactive reads expose no ownership or shutdown claim.
#[async_trait::async_trait]
impl QueryRead for &Client {
    async fn query(&mut self, sql: &str, row_limit: usize) -> Result<Vec<Row>> {
        crate::rows::checked_query(self, sql, row_limit).await
    }
    async fn query_with_state(&mut self, sql: &str, row_limit: usize) -> NativeResult<Vec<Row>> {
        crate::rows::checked_query_with_state(self, sql, row_limit).await
    }
    async fn batch_execute(&mut self, sql: &str) -> Result<()> {
        Client::batch_execute(self, &crate::rows::tagged(sql)).await.map_err(crate::connect::map_error)
    }
    async fn query_text_array(
        &mut self,
        sql: &str,
        values: Vec<String>,
        _row_limit: usize,
    ) -> Result<Vec<tokio_postgres::Row>> {
        Client::query(self, crate::rows::tagged(sql).as_str(), &[&values]).await.map_err(crate::connect::map_error)
    }
}
#[async_trait::async_trait]
pub trait NativeRead: QueryRead {
    fn queries(&self) -> u32;
    fn metrics(&self) -> Option<ReadWorkMetrics>;
    async fn close(&mut self) -> SourceShutdown;
}

pub struct ReadSession {
    connector: Connector,
    client: Option<Client>,
    streaming: Option<StreamingRead>,
    active: Option<JoinHandle<CompletedRead>>,
    budget: CollectionBudget,
    deadline: Instant,
    queries: u32,
    acquiring: bool,
    initialized: bool,
    setup: SessionSetup,
    application_scope: Option<ApplicationReadScope>,
    closed: bool,
    fence: culprit_read::ReadFence,
    metrics: ReadMetricsRecorder,
    notices: Option<std::sync::Arc<std::sync::atomic::AtomicU64>>,
}
#[derive(Clone, Copy)]
enum SessionSetup {
    Telemetry,
    Application(SessionLimits),
}
struct StreamingRead {
    _owner: tokio::sync::oneshot::Sender<()>,
    receiver: tokio::sync::mpsc::Receiver<ReadBatch<Row>>,
}
impl ReadSession {
    pub(crate) fn new(connector: Connector, budget: CollectionBudget) -> Self {
        Self::for_engine(connector, budget, Engine::Postgres)
    }
    /// Shared wire ownership only. The caller supplies its engine for SQL redaction/accounting and owns every
    /// source query, scope, identity and counter interpretation; this never selects PostgreSQL collectors.
    pub fn for_engine(connector: Connector, budget: CollectionBudget, engine: Engine) -> Self {
        let fence = connector.fence.clone();
        Self {
            connector,
            client: None,
            streaming: None,
            active: None,
            deadline: Instant::now() + Duration::from_millis(budget.total_timeout_ms.into()),
            budget,
            queries: 0,
            acquiring: false,
            initialized: false,
            setup: SessionSetup::Telemetry,
            application_scope: None,
            closed: false,
            fence,
            metrics: ReadMetricsRecorder::new(engine),
            notices: None,
        }
    }
    /// Normal acquisition keeps its existing native SET batch and per-query retention; only the explicitly
    /// shared query/time allowance is new. Collector and YSQL setup policies are unchanged.
    pub(crate) fn application(connector: Connector, limits: SessionLimits, scope: ApplicationReadScope) -> Self {
        let budget = CollectionBudget {
            max_queries: application::APPLICATION_QUERY_LIMIT,
            page_bytes: culprit_core::limits::MAX_RESULT_BYTES as u32,
            statement_timeout_ms: limits.statement_timeout.as_millis() as u32,
            total_timeout_ms: application::APPLICATION_DURATION.as_millis() as u32,
            ..CollectionBudget::default()
        };
        let mut reader = Self::new(connector, budget);
        reader.deadline = scope.deadline;
        reader.setup = SessionSetup::Application(limits);
        reader.application_scope = Some(scope);
        reader
    }
    fn setup_sql(&self) -> String {
        match self.setup {
            SessionSetup::Application(limits) => crate::connect::session_setup(limits),
            SessionSetup::Telemetry => {
                let mut setup = crate::connect::session_setup(SessionLimits {
                    statement_timeout: Duration::from_millis(self.budget.statement_timeout_ms.into()),
                    catalog_path: true,
                });
                setup.push_str(
                    "; SET max_parallel_workers_per_gather = 0 /* culprit */; SET work_mem = '4MB' /* culprit */; SET extra_float_digits = 3 /* culprit */",
                );
                setup
            }
        }
    }
    pub(crate) fn under_scope(mut self, scope: ApplicationReadScope) -> Self {
        self.deadline = self.deadline.min(scope.deadline);
        self.application_scope = Some(scope);
        self
    }
    pub(crate) async fn finish<T>(mut self, result: Result<T>) -> Result<T> {
        if self.close().await == SourceShutdown::Unconfirmed {
            return Err(CollectionStop::CancellationUnconfirmed.error());
        }
        match result {
            Ok(value) => {
                if let Some(scope) = &self.application_scope {
                    scope.finish()?;
                }
                Ok(value)
            }
            Err(error) => Err(error),
        }
    }
    fn remaining(&self) -> Result<Duration> {
        let time = self
            .deadline
            .saturating_duration_since(Instant::now())
            .min(Duration::from_millis(self.budget.statement_timeout_ms.into()));
        if time.is_zero() { Err(CollectionStop::TimedOut.error()) } else { Ok(time) }
    }
    async fn initialize(&mut self) -> Result<()> {
        if !self.initialized {
            let setup = self.setup_sql();
            self.execute(&setup, 0).await?;
            self.initialized = true;
        }
        Ok(())
    }
    pub async fn begin_stream(&mut self, sql: &str, row_limit: usize) -> Result<()> {
        self.initialize().await?;
        let (output, receiver) = RowOutput::streaming(self.budget.page_rows as usize, self.budget.page_bytes as usize);
        let owner = self.start(sql, row_limit, self.budget.total_bytes as usize, output).await?;
        self.streaming = Some(StreamingRead { _owner: owner, receiver });
        Ok(())
    }
    pub async fn next_batch(&mut self) -> Result<ReadBatch<Row>> {
        let batch = self.streaming.as_mut().ok_or_else(|| CollectionStop::InvalidPage.error())?.receiver.recv().await;
        match batch {
            Some(batch) if !batch.complete => Ok(batch),
            Some(batch) => {
                let result = self.finish_active().await;
                self.streaming.take();
                result.map_err(|failure| failure.error)?;
                Ok(batch)
            }
            None => {
                let result = self.finish_active().await;
                self.streaming.take();
                result.map_err(|failure| failure.error)?;
                Err(CollectionStop::InvalidPage.error())
            }
        }
    }
    async fn acquire(&mut self) -> Result<Client> {
        let timeout = self.remaining()?;
        self.acquiring = true;
        let result = tokio::time::timeout(timeout, self.connector.connect_unconfigured()).await;
        self.acquiring = false;
        match result {
            Ok(result) => result.map(|session| {
                self.notices = Some(session.notices.clone());
                session.client
            }),
            Err(_) => {
                self.fence.mark_uncertain();
                Err(CollectionStop::CancellationUnconfirmed.error())
            }
        }
    }
    /// Native notices can mean a distributed reader skipped a node. No server-controlled text is retained.
    pub fn notices(&self) -> u64 {
        self.notices.as_ref().map_or(0, |count| count.load(Ordering::SeqCst))
    }
    async fn start(
        &mut self,
        sql: &str,
        row_limit: usize,
        byte_limit: usize,
        output: RowOutput<Row>,
    ) -> Result<tokio::sync::oneshot::Sender<()>> {
        self.start_dispatch(sql, row_limit, byte_limit, ReadOutput::Text(output), owned::ReadDispatch::Simple)
            .await
            .map_err(|failure| failure.error)
    }
    async fn start_dispatch(
        &mut self,
        sql: &str,
        row_limit: usize,
        byte_limit: usize,
        output: ReadOutput,
        dispatch: owned::ReadDispatch,
    ) -> NativeResult<tokio::sync::oneshot::Sender<()>> {
        self.admit()?;
        let client = match self.client.take() {
            Some(client) => client,
            None => self.acquire().await?,
        };
        let timeout = self.remaining()?;
        self.queries += 1;
        let (owner, cancellation) = tokio::sync::oneshot::channel();
        let template = self.metrics.begin(sql);
        self.active = Some(tokio::spawn(
            owned::ReadTask {
                client,
                connector: self.connector.clone(),
                request: owned::ReadRequest { sql: sql.to_owned(), row_limit, byte_limit, timeout, dispatch },
                metrics: self.metrics.clone(),
                template,
                fence: self.fence.clone(),
            }
            .run(output, cancellation),
        ));

        Ok(owner)
    }
    async fn execute(&mut self, sql: &str, row_limit: usize) -> Result<Vec<Row>> {
        self.execute_text(sql, row_limit).await.map_err(|failure| failure.error)
    }
    async fn execute_text(&mut self, sql: &str, row_limit: usize) -> NativeResult<Vec<Row>> {
        let owner = self
            .start_dispatch(
                sql,
                row_limit,
                self.budget.page_bytes as usize,
                ReadOutput::Text(RowOutput::buffered()),
                owned::ReadDispatch::Simple,
            )
            .await?;
        let result = self.finish_active().await;
        drop(owner);
        match result? {
            ReadRows::Text(rows) => Ok(rows),
            ReadRows::Native(_) => Err(CollectionStop::InvalidPage.error().into()),
        }
    }
    async fn execute_with_state(&mut self, sql: &str, row_limit: usize) -> NativeResult<Vec<Row>> {
        self.initialize().await?;
        self.execute_text(sql, row_limit).await
    }
    async fn execute_text_array(
        &mut self,
        sql: &str,
        values: Vec<String>,
        row_limit: usize,
    ) -> NativeResult<Vec<tokio_postgres::Row>> {
        self.initialize().await?;
        let owner = self
            .start_dispatch(
                sql,
                row_limit,
                self.budget.page_bytes as usize,
                ReadOutput::Native(RowOutput::buffered()),
                owned::ReadDispatch::TextArray(values),
            )
            .await?;
        let result = self.finish_active().await;
        drop(owner);
        match result? {
            ReadRows::Native(rows) => Ok(rows),
            ReadRows::Text(_) => Err(CollectionStop::InvalidPage.error().into()),
        }
    }
    fn admit(&self) -> Result<()> {
        if self.closed || self.acquiring || self.active.is_some() || self.fence.is_uncertain() {
            return Err(CollectionStop::CancellationUnconfirmed.error());
        }
        if let Some(scope) = &self.application_scope {
            scope.ensure_available()?;
        }
        if self.queries >= self.budget.max_queries {
            return Err(CollectionStop::QueryLimit.error());
        }
        self.remaining()?;
        if let Some(scope) = &self.application_scope {
            scope.start()?;
        }
        Ok(())
    }
    async fn finish_active(&mut self) -> NativeResult<ReadRows> {
        let completed = self.active.as_mut().expect("a native read is owned").await;
        self.active.take();
        match completed {
            Ok(completed) if completed.confirmed => {
                if completed.client.is_none() {
                    self.initialized = false;
                    // An interrupted normal session is retired, never replaced by a new login or a late cancel race.
                    self.closed |= self.application_scope.is_some();
                }
                self.client = completed.client;
                completed.result
            }
            _ => {
                self.fence.mark_uncertain();
                Err(CollectionStop::CancellationUnconfirmed.error().into())
            }
        }
    }
}
#[async_trait::async_trait]
impl QueryRead for ReadSession {
    async fn query(&mut self, sql: &str, row_limit: usize) -> Result<Vec<Row>> {
        self.initialize().await?;
        self.execute(sql, row_limit).await
    }
    async fn query_with_state(&mut self, sql: &str, row_limit: usize) -> NativeResult<Vec<Row>> {
        self.execute_with_state(sql, row_limit).await
    }
    async fn query_text_array(
        &mut self,
        sql: &str,
        values: Vec<String>,
        row_limit: usize,
    ) -> Result<Vec<tokio_postgres::Row>> {
        self.execute_text_array(sql, values, row_limit).await.map_err(|failure| failure.error)
    }
}

pub async fn query(reader: &mut (impl QueryRead + ?Sized), sql: &str) -> Result<Vec<Row>> {
    reader.query(sql, culprit_core::limits::MAX_RESULT_ROWS).await
}
pub async fn query_one(reader: &mut (impl QueryRead + ?Sized), sql: &str) -> Result<Option<Row>> {
    let mut rows = reader.query(sql, 2).await?;
    if rows.len() > 1 {
        return Err(CollectionStop::InvalidPage.error());
    }
    Ok(rows.pop())
}
pub async fn query_bounded(
    reader: &mut (impl QueryRead + ?Sized),
    sql: &str,
    limit: usize,
) -> Result<(Vec<Row>, bool)> {
    let mut rows = reader.query(sql, limit.saturating_add(1)).await?;
    let truncated = rows.len() > limit;
    rows.truncate(limit);
    Ok((rows, truncated))
}
#[async_trait::async_trait]
impl NativeRead for ReadSession {
    fn queries(&self) -> u32 {
        self.queries
    }
    fn metrics(&self) -> Option<ReadWorkMetrics> {
        self.metrics.snapshot()
    }
    async fn close(&mut self) -> SourceShutdown {
        self.closed = true;
        self.streaming.take();
        if self.acquiring {
            self.fence.mark_uncertain();
        }
        if self.active.is_some() {
            let _ = self.finish_active().await;
        }
        self.client.take();
        if self.fence.is_uncertain() { SourceShutdown::Unconfirmed } else { SourceShutdown::Quiescent }
    }
}
impl Drop for ReadSession {
    fn drop(&mut self) {
        if self.acquiring || self.active.is_some() {
            self.fence.mark_uncertain();
        }
        // Dropping a JoinHandle detaches the owned task. Its caller token requests cancellation, and the task keeps
        // both the client and SSH/TLS route until protocol completion or an explicitly unconfirmed shutdown.
    }
}
