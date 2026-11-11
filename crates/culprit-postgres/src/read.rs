//! One owner for bounded native reads. Caller cancellation never abandons a response mid-protocol.
#[cfg(test)]
mod native_tests;
mod owned;
use crate::{
    connect::{Connector, SessionLimits},
    rows::Row,
};
use culprit_core::{
    Engine, Result,
    telemetry::{CollectionBudget, CollectionStop, ReadMetricsRecorder, ReadWorkMetrics, SourceShutdown},
};
use culprit_read::{ReadBatch, RowOutput};
use std::{
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};
use tokio::{task::JoinHandle, time::Instant};
use tokio_postgres::Client;

pub(crate) struct CompletedRead {
    client: Option<Client>,
    result: Result<Vec<Row>>,
    confirmed: bool,
}
#[async_trait::async_trait]
pub trait QueryRead: Send {
    async fn query(&mut self, sql: &str, row_limit: usize) -> Result<Vec<Row>>;
}
/// Borrowed interactive reads expose no ownership or shutdown claim.
#[async_trait::async_trait]
impl QueryRead for &Client {
    async fn query(&mut self, sql: &str, row_limit: usize) -> Result<Vec<Row>> {
        let (rows, truncated) = crate::rows::query_bounded(self, sql, row_limit).await?;
        if truncated {
            return Err(CollectionStop::RowLimit.error());
        }
        Ok(rows)
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
    closed: bool,
    fence: culprit_read::ReadFence,
    metrics: ReadMetricsRecorder,
    notices: Option<std::sync::Arc<std::sync::atomic::AtomicU64>>,
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
            closed: false,
            fence,
            metrics: ReadMetricsRecorder::new(engine),
            notices: None,
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
            let mut setup = crate::connect::session_setup(SessionLimits {
                statement_timeout: Duration::from_millis(self.budget.statement_timeout_ms.into()),
                catalog_path: true,
            });
            setup.push_str(
                "; SET max_parallel_workers_per_gather = 0 /* culprit */; SET work_mem = '4MB' /* culprit */; SET extra_float_digits = 3 /* culprit */",
            );
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
                result?;
                Ok(batch)
            }
            None => {
                let result = self.finish_active().await;
                self.streaming.take();
                result?;
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
                request: owned::ReadRequest { sql: sql.to_owned(), row_limit, byte_limit, timeout },
                metrics: self.metrics.clone(),
                template,
                fence: self.fence.clone(),
            }
            .run(output, cancellation),
        ));

        Ok(owner)
    }
    async fn execute(&mut self, sql: &str, row_limit: usize) -> Result<Vec<Row>> {
        let owner = self.start(sql, row_limit, self.budget.page_bytes as usize, RowOutput::buffered()).await?;
        let result = self.finish_active().await;
        drop(owner);
        result
    }
    fn admit(&self) -> Result<()> {
        if self.closed || self.acquiring || self.active.is_some() || self.fence.is_uncertain() {
            return Err(CollectionStop::CancellationUnconfirmed.error());
        }
        if self.queries >= self.budget.max_queries {
            return Err(CollectionStop::QueryLimit.error());
        }
        self.remaining().map(|_| ())
    }
    async fn finish_active(&mut self) -> Result<Vec<Row>> {
        let completed = self.active.as_mut().expect("a native read is owned").await;
        self.active.take();
        match completed {
            Ok(completed) if completed.confirmed => {
                if completed.client.is_none() {
                    self.initialized = false;
                }
                self.client = completed.client;
                completed.result
            }
            _ => {
                self.fence.mark_uncertain();
                Err(CollectionStop::CancellationUnconfirmed.error())
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
