//! Bounded native ownership. Snapshot/Live projections and telemetry share source queries over this interface.
pub(crate) mod accounting;
#[cfg(test)]
mod native_tests;
mod owned;
use accounting::Accounting;

use crate::{
    connect::{Connector, Scope},
    rows::Rec,
};
use culprit_core::{Engine, Result, telemetry::*};
use culprit_read::{ReadBatch, RowOutput};
use mysql_async::{Conn, Value, prelude::Queryable};
use std::{
    sync::{
        Arc,
        atomic::{AtomicU32, Ordering},
    },
    time::Duration,
};
use tokio::{
    sync::{mpsc, oneshot},
    task::JoinHandle,
    time::Instant,
};

#[async_trait::async_trait]
pub(crate) trait QueryRead: Send {
    async fn read(&mut self, sql: &str, parameters: Vec<Value>, row_limit: usize) -> Result<Vec<Rec>>;
    async fn explain(&mut self, sql: &str) -> Result<Option<Rec>>;
}

/// Borrowed reads share layouts/decoders without claiming ownership of an interactive caller's connection.
#[async_trait::async_trait]
impl QueryRead for Conn {
    async fn explain(&mut self, sql: &str) -> Result<Option<Rec>> {
        match self.exec_first::<mysql_async::Row, _, _>(crate::rows::tagged(sql), ()).await {
            Ok(row) => row.map(Rec::from_native).transpose(),
            Err(mysql_async::Error::Server(error)) if error.code == 1295 => {
                let mut rows = self.read(sql, vec![], 1).await?;
                Ok(rows.pop())
            }
            Err(error) => Err(crate::connect::map_error(error)),
        }
    }
    async fn read(&mut self, sql: &str, parameters: Vec<Value>, row_limit: usize) -> Result<Vec<Rec>> {
        let progress = ReadProgress::default();
        let (limits, _) = mpsc::channel(1);
        let request =
            owned::Request { sql: sql.into(), parameters, row_limit, byte_limit: 8 * 1024 * 1024, prepared: false };
        if request.parameters.is_empty() {
            let result = self.query_iter(crate::rows::tagged(sql)).await.map_err(crate::connect::map_error)?;
            owned::drain(result, &request, &progress, limits, RowOutput::buffered())
                .await
                .map_err(crate::connect::map_error)?
        } else {
            let result = self
                .exec_iter(crate::rows::tagged(sql), request.parameters.clone())
                .await
                .map_err(crate::connect::map_error)?;
            owned::drain(result, &request, &progress, limits, RowOutput::buffered())
                .await
                .map_err(crate::connect::map_error)?
        }
    }
}

pub(super) struct Peer {
    client: Conn,
    control: Conn,
    initialized: bool,
}
pub(super) struct Reply<T = Vec<Rec>> {
    peer: Option<Peer>,
    result: Result<T>,
    shutdown: SourceShutdown,
}
struct Streaming {
    _owner: oneshot::Sender<()>,
    receiver: mpsc::Receiver<ReadBatch<Rec>>,
}
pub(crate) struct ReadSession {
    connector: Arc<Connector>,
    scope: Scope,
    peer: Option<Peer>,
    active: Option<JoinHandle<Reply>>,
    streaming: Option<Streaming>,
    accounting: Accounting,
    closed: bool,
}
impl ReadSession {
    /// Manual scans collect several independent source groups. Their total work is bounded here, and every query
    /// uses the same native owner as scheduled collection. SQL/plan enrichment stays an explicit scan choice.
    pub(crate) fn application(connector: Arc<Connector>, scope: Scope) -> Self {
        Self::application_window(connector, scope, Duration::ZERO)
    }
    pub(crate) fn application_window(connector: Arc<Connector>, scope: Scope, window: Duration) -> Self {
        Self::new(
            connector,
            CollectionBudget {
                max_snapshot_rows: 10_000,
                max_queries: 512,
                total_timeout_ms: (window.as_millis() + 60_000).min(900_000) as u32,
                page_bytes: 8 * 1024 * 1024,
                statement_timeout_ms: culprit_core::limits::OBSERVE_TIMEOUT.as_millis() as u32,
                ..Default::default()
            },
            scope,
        )
    }
    pub(crate) fn new(connector: Arc<Connector>, budget: CollectionBudget, scope: Scope) -> Self {
        Self {
            connector,
            scope,
            peer: None,
            active: None,
            streaming: None,
            closed: false,
            accounting: Accounting::new(budget),
        }
    }
    async fn start(&mut self, request: owned::Request, output: RowOutput<Rec>) -> Result<oneshot::Sender<()>> {
        self.connector.fence.ensure_available()?;
        if self.closed || self.active.is_some() {
            return Err(CollectionStop::InvalidPage.error());
        }
        self.accounting.remaining()?;
        if self.query_count() >= self.accounting.budget.max_queries {
            return Err(CollectionStop::QueryLimit.error());
        }
        let (owner, cancellation) = oneshot::channel();
        self.active = Some(tokio::spawn(
            owned::Task {
                peer: self.peer.take(),
                connector: self.connector.clone(),
                scope: self.scope,
                operation: owned::Rows { request, output },
                accounting: self.accounting.clone(),
            }
            .run(cancellation),
        ));
        Ok(owner)
    }
    pub(crate) async fn begin_stream(&mut self, sql: &str, parameters: Vec<Value>, row_limit: usize) -> Result<()> {
        let budget = &self.accounting.budget;
        let (output, receiver) = RowOutput::streaming(budget.page_rows as usize, budget.page_bytes as usize);
        let request = owned::Request {
            sql: sql.into(),
            parameters,
            row_limit,
            byte_limit: budget.total_bytes as usize,
            prepared: false,
        };
        let owner = self.start(request, output).await?;
        self.streaming = Some(Streaming { _owner: owner, receiver });
        Ok(())
    }
    pub(crate) async fn next_batch(&mut self) -> Result<ReadBatch<Rec>> {
        let batch = self.streaming.as_mut().ok_or_else(|| CollectionStop::InvalidPage.error())?.receiver.recv().await;
        match batch {
            Some(batch) if !batch.complete => Ok(batch),
            Some(batch) => {
                self.finish_active().await?;
                self.streaming.take();
                Ok(batch)
            }
            None => {
                self.finish_active().await?;
                self.streaming.take();
                Err(CollectionStop::InvalidPage.error())
            }
        }
    }
    async fn finish_active(&mut self) -> Result<Vec<Rec>> {
        let reply = self.active.as_mut().ok_or_else(|| CollectionStop::InvalidPage.error())?.await;
        self.active.take();
        match reply {
            Ok(reply) if reply.shutdown == SourceShutdown::Quiescent => {
                self.peer = reply.peer;
                reply.result
            }
            _ => {
                self.connector.fence.mark_uncertain();
                Err(CollectionStop::CancellationUnconfirmed.error())
            }
        }
    }
    pub(crate) fn query_count(&self) -> u32 {
        self.accounting.queries.load(Ordering::Acquire)
    }
    pub(crate) fn read_metrics(&self) -> Option<ReadWorkMetrics> {
        self.accounting.metrics.snapshot()
    }
    pub(crate) async fn close(&mut self) -> SourceShutdown {
        self.closed = true;
        self.streaming.take();
        if self.active.is_some() {
            let _ = self.finish_active().await;
        }
        if let Some(peer) = self.peer.take() {
            peer.close().await;
        }
        self.connector.source_shutdown()
    }
}
#[async_trait::async_trait]
impl QueryRead for ReadSession {
    async fn explain(&mut self, sql: &str) -> Result<Option<Rec>> {
        let request = owned::Request {
            sql: sql.into(),
            parameters: vec![],
            row_limit: 1,
            byte_limit: self.accounting.budget.page_bytes as usize,
            prepared: true,
        };
        let owner = self.start(request, RowOutput::buffered()).await?;
        let result = self.finish_active().await;
        drop(owner);
        result.map(|mut rows| rows.pop())
    }
    async fn read(&mut self, sql: &str, parameters: Vec<Value>, row_limit: usize) -> Result<Vec<Rec>> {
        let request = owned::Request {
            sql: sql.into(),
            parameters,
            row_limit,
            byte_limit: self.accounting.budget.page_bytes as usize,
            prepared: false,
        };
        let owner = self.start(request, RowOutput::buffered()).await?;
        let result = self.finish_active().await;
        drop(owner);
        result
    }
}
impl Drop for ReadSession {
    fn drop(&mut self) {
        if self.active.is_some() {
            self.connector.fence.mark_uncertain();
        }
        // The detached native task retains the clients and route. Releasing the query token asks it to retire;
        // missing proof permanently fences the connector rather than admitting a replacement query.
    }
}
impl Peer {
    async fn close(self) {
        let timeout = Duration::from_secs(1);
        let _ = tokio::join!(
            tokio::time::timeout(timeout, self.client.disconnect()),
            tokio::time::timeout(timeout, self.control.disconnect())
        );
    }
}

pub(crate) async fn preview(
    connector: Arc<Connector>,
    sql: String,
    limit: usize,
    permit: tokio::sync::SemaphorePermit<'static>,
) -> Result<culprit_core::ResultSet> {
    let caller = connector.fence.start_work()?;
    let (owner, cancellation) = oneshot::channel();
    let task = tokio::spawn(async move {
        let _permit = permit;
        preview_work(connector, None, sql, limit, None, cancellation).await
    });
    let reply = task.await.map_err(|_| {
        culprit_core::Error::internal(culprit_core::t!("provider.mysql.preview.worker_stopped"))
            .with_detail(culprit_core::ErrorDetail::CollectionStopped(CollectionStop::CancellationUnconfirmed))
    })?;
    drop(owner);
    caller.complete(reply.shutdown);
    reply.result
}
async fn preview_work(
    connector: Arc<Connector>,
    peer: Option<Peer>,
    sql: String,
    limit: usize,
    setting: Option<String>,
    cancellation: oneshot::Receiver<()>,
) -> Reply<culprit_core::ResultSet> {
    let budget = CollectionBudget {
        statement_timeout_ms: culprit_core::limits::OBSERVE_TIMEOUT.as_millis() as u32,
        ..Default::default()
    };
    let started = std::time::Instant::now();
    let mut reply = owned::Task {
        connector,
        peer,
        scope: Scope::Database,
        operation: owned::Preview { sql, limit, setting },
        accounting: Accounting::new(budget),
    }
    .run(cancellation)
    .await;
    if let Some(peer) = reply.peer.take() {
        peer.close().await;
    }
    reply.result = reply.result.map_err(|error| match CollectionStop::from_error(&error) {
        CollectionStop::Cancelled if reply.shutdown == SourceShutdown::Quiescent => {
            culprit_core::limits::timeout_error()
        }
        CollectionStop::CancellationUnconfirmed => culprit_core::Error::new(
            culprit_core::ErrorCode::Timeout,
            culprit_core::t!("provider.mysql.preview.cancel_unconfirmed"),
        )
        .with_detail(culprit_core::ErrorDetail::CollectionStopped(CollectionStop::CancellationUnconfirmed)),
        _ => error,
    });
    if let Ok(result) = &mut reply.result {
        result.elapsed_ms = started.elapsed().as_secs_f64() * 1000.0;
    }
    reply
}
#[cfg(test)]
pub(crate) async fn preview_with_clients(
    connector: Arc<Connector>,
    client: Conn,
    control: Conn,
    sql: &str,
    limit: usize,
    setting: &str,
    cancellation: oneshot::Receiver<()>,
) -> Result<culprit_core::ResultSet> {
    preview_work(
        connector,
        Some(Peer { client, control, initialized: true }),
        sql.into(),
        limit,
        Some(setting.into()),
        cancellation,
    )
    .await
    .result
}
