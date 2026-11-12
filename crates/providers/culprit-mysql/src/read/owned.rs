use super::*;
use crate::{
    cancellation,
    connect::{Flavor, SESSION_SETUP, map_error},
};
use mysql_async::{QueryResult, prelude::Protocol};
use std::{
    future::{Future, poll_fn},
    sync::atomic::AtomicBool,
};

const ACKNOWLEDGEMENT_TIMEOUT: Duration = Duration::from_secs(5);

pub(super) struct Request {
    pub sql: String,
    pub parameters: Vec<Value>,
    pub row_limit: usize,
    pub byte_limit: usize,
    pub prepared: bool,
}
pub(super) struct Task<O> {
    pub peer: Option<Peer>,
    pub connector: Arc<Connector>,
    pub scope: Scope,
    pub operation: O,
    pub accounting: Accounting,
}
#[async_trait::async_trait]
pub(super) trait Operation: Send {
    type Output: Send;
    fn requires_server_timeout(&self) -> bool {
        true
    }
    async fn execute(self, client: &mut Conn, phase: &mut Phase<'_>, connector: &Connector) -> Result<Self::Output>;
}
pub(super) struct Rows {
    pub request: Request,
    pub output: RowOutput<Rec>,
}
#[async_trait::async_trait]
impl Operation for Rows {
    type Output = Vec<Rec>;
    async fn execute(self, client: &mut Conn, phase: &mut Phase<'_>, _connector: &Connector) -> Result<Self::Output> {
        phase.rows(client, &self.request, self.output).await
    }
}
impl<O: Operation> Task<O> {
    pub(super) async fn run(mut self, mut cancellation: oneshot::Receiver<()>) -> Reply<O::Output> {
        let work = match self.connector.fence.start_work() {
            Ok(work) => work,
            Err(error) => return Reply { peer: None, result: Err(error), shutdown: SourceShutdown::Unconfirmed },
        };
        let mut shutdown = SourceShutdown::Quiescent;
        let mut interrupted = false;
        let peer = match self.peer.take() {
            Some(peer) => Ok(peer),
            None => self.acquire(&mut cancellation, &mut shutdown).await,
        };
        let mut peer = match peer {
            Ok(peer) => peer,
            Err(error) => {
                work.complete(shutdown);
                return Reply { peer: None, result: Err(error), shutdown };
            }
        };
        let marker = marker();
        let mut phase = Phase {
            control: &mut peer.control,
            cancellation: &mut cancellation,
            marker: &marker,
            connection_id: peer.client.id(),
            accounting: &self.accounting,
            shutdown: &mut shutdown,
            interrupted: &mut interrupted,
        };
        let result = async {
            if !peer.initialized {
                initialize(
                    &mut peer.client,
                    &self.connector,
                    self.scope,
                    self.operation.requires_server_timeout(),
                    &mut phase,
                )
                .await?;
                peer.initialized = true;
            }
            self.operation.execute(&mut peer.client, &mut phase, &self.connector).await
        }
        .await;
        // A drained native ERR (for example an optional source denied by permissions) still leaves a synchronized
        // client. Interrupted clients are always discarded, even after a confirmed cancellation.
        let keep = !interrupted && shutdown == SourceShutdown::Quiescent;
        let peer = if keep {
            Some(peer)
        } else {
            peer.close().await;
            None
        };
        work.complete(shutdown);
        Reply { peer, result, shutdown }
    }
    async fn acquire(&self, cancellation: &mut oneshot::Receiver<()>, shutdown: &mut SourceShutdown) -> Result<Peer> {
        // Both raw connections perform handshake only. Every SQL setup statement below is an owned phase.
        // The control route remains owned even if a caller disappears during original-response retirement.
        let connect = async {
            let client = self.connector.connect_unconfigured().await?;
            let control = self.connector.connect_for_cancellation().await?;
            Ok(Peer { client, control, initialized: false })
        };
        let timeout = self.accounting.remaining()?;
        tokio::select! {
            biased;
            _ = cancellation => {
                *shutdown = SourceShutdown::Unconfirmed;
                Err(CollectionStop::CancellationUnconfirmed.error())
            }
            result = tokio::time::timeout(timeout, connect) => match result {
                Ok(result) => result,
                Err(_) => {
                    *shutdown = SourceShutdown::Unconfirmed;
                    Err(CollectionStop::CancellationUnconfirmed.error())
                }
            }
        }
    }
}

fn marker() -> String {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let epoch = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos();
    format!(
        "{} /* read:{:x}-{epoch:x}-{:x} */",
        crate::connect::TAG,
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}

pub(super) struct Phase<'a> {
    control: &'a mut Conn,
    cancellation: &'a mut oneshot::Receiver<()>,
    marker: &'a str,
    connection_id: u32,
    accounting: &'a Accounting,
    shutdown: &'a mut SourceShutdown,
    interrupted: &'a mut bool,
}
impl Phase<'_> {
    async fn optional_setup(&mut self, client: &mut Conn, sql: String) -> Result<()> {
        match self.setup(client, sql).await {
            Err(error)
                if *self.shutdown == SourceShutdown::Quiescent
                    && !*self.interrupted
                    && error.code != culprit_core::ErrorCode::Timeout =>
            {
                Ok(())
            }
            result => result,
        }
    }
    async fn run<T: Send>(
        &mut self,
        sql: &str,
        work: impl Future<Output = mysql_async::Result<Result<T>>>,
        mut limits: mpsc::Receiver<CollectionStop>,
        progress: &ReadProgress,
        retention: culprit_read::RetentionControl,
    ) -> Result<T> {
        self.accounting.admit()?;
        let timeout = self.accounting.remaining()?;
        let template = self.accounting.metrics.begin(sql);
        let started_at = std::time::Instant::now();
        let started = AtomicBool::new(false);
        tokio::pin!(work);
        let response = poll_fn(|cx| {
            started.store(true, Ordering::Release);
            work.as_mut().poll(cx)
        });
        tokio::pin!(response);
        let completed = tokio::select! {
            biased;
            _ = &mut *self.cancellation => Err(CollectionStop::Cancelled),
            _ = tokio::time::sleep(timeout) => Err(CollectionStop::TimedOut),
            Some(reason) = limits.recv() => Err(reason),
            result = &mut response => Ok(result),
        };
        let (result, outcome) = match completed {
            Ok(result) => {
                if !cancellation::response_completed(&result) {
                    *self.shutdown = SourceShutdown::Unconfirmed;
                }
                let result = if *self.shutdown == SourceShutdown::Unconfirmed {
                    Err(CollectionStop::CancellationUnconfirmed.error())
                } else {
                    result.map_err(map_error).and_then(|result| result)
                };
                let outcome = ReadTaskOutcome {
                    failed: result.is_err(),
                    timed_out: result
                        .as_ref()
                        .err()
                        .is_some_and(|error| error.code == culprit_core::ErrorCode::Timeout),
                    unconfirmed: *self.shutdown == SourceShutdown::Unconfirmed,
                    ..Default::default()
                };
                (result, outcome)
            }
            Err(reason) => {
                retention.discard();
                *self.interrupted = true;
                if started.load(Ordering::Acquire) {
                    *self.shutdown = cancellation::settle(
                        response,
                        cancellation::request(self.control, self.connection_id, self.marker, self.accounting),
                        ACKNOWLEDGEMENT_TIMEOUT,
                    )
                    .await;
                }
                let unconfirmed = *self.shutdown == SourceShutdown::Unconfirmed;
                let error = if unconfirmed { CollectionStop::CancellationUnconfirmed } else { reason };
                (
                    Err(error.error()),
                    ReadTaskOutcome {
                        failed: true,
                        timed_out: reason == CollectionStop::TimedOut,
                        cancelled: reason == CollectionStop::Cancelled,
                        unconfirmed,
                    },
                )
            }
        };
        self.accounting.metrics.finish(template.as_deref(), started_at.elapsed(), progress, &outcome);
        result
    }
    async fn rows(&mut self, client: &mut Conn, request: &Request, output: RowOutput<Rec>) -> Result<Vec<Rec>> {
        let retention = output.retention_control();
        let sql = format!(
            "{} /* culprit:monitor */ /* culprit:template:{} */ {}",
            self.marker,
            culprit_core::telemetry::digest(request.sql.as_bytes()),
            request.sql
        );
        if request.parameters.is_empty() && !request.prepared {
            let progress = ReadProgress::default();
            let (limits, receiver) = mpsc::channel(1);
            let work = async {
                let result = client.query_iter(sql).await?;
                drain(result, request, &progress, limits, output).await
            };
            self.run(&request.sql, work, receiver, &progress, retention).await
        } else {
            // Preparation and execution are separate phases: cancellation cannot finish PREPARE and then dispatch
            // a fresh EXECUTE while consuming the original response for shutdown confirmation.
            let progress = ReadProgress::default();
            let (_, receiver) = mpsc::channel(1);
            let statement = self
                .run(&request.sql, async { client.prep(sql).await.map(Ok) }, receiver, &progress, retention.clone())
                .await?;
            let progress = ReadProgress::default();
            let (limits, receiver) = mpsc::channel(1);
            let work = async {
                let result = client.exec_iter(&statement, request.parameters.clone()).await?;
                drain(result, request, &progress, limits, output).await
            };
            self.run(&request.sql, work, receiver, &progress, retention).await
        }
    }
    async fn setup(&mut self, client: &mut Conn, sql: String) -> Result<()> {
        self.rows(
            client,
            &Request {
                sql,
                parameters: vec![],
                row_limit: 1,
                byte_limit: self.accounting.budget.page_bytes as usize,
                prepared: false,
            },
            RowOutput::buffered(),
        )
        .await
        .map(|_| ())
    }
}

pub(super) struct Preview {
    pub sql: String,
    pub limit: usize,
    pub setting: Option<String>,
}
#[async_trait::async_trait]
impl Operation for Preview {
    type Output = culprit_core::ResultSet;
    fn requires_server_timeout(&self) -> bool {
        false
    }
    async fn execute(self, client: &mut Conn, phase: &mut Phase<'_>, connector: &Connector) -> Result<Self::Output> {
        let setting =
            self.setting.unwrap_or_else(|| connector.flavor().timeout_for(culprit_core::limits::PREVIEW_TIMEOUT));
        phase.optional_setup(client, setting).await?;
        phase.setup(client, "START TRANSACTION READ ONLY".into()).await?;
        let progress = ReadProgress::default();
        let (_, limits) = mpsc::channel(1);
        let sql = format!("{} {}", phase.marker, self.sql);
        // Setup has its own allowance; this phase starts the short preview execution budget after setup settles.
        let budget = culprit_core::limits::PREVIEW_TIMEOUT + Duration::from_millis(250);
        let accounting = phase.accounting;
        let result = {
            let preview_accounting = Accounting {
                budget: CollectionBudget {
                    statement_timeout_ms: budget.as_millis() as u32,
                    ..accounting.budget.clone()
                },
                ..accounting.clone()
            };
            let mut query_phase = Phase {
                control: phase.control,
                cancellation: phase.cancellation,
                marker: phase.marker,
                connection_id: phase.connection_id,
                accounting: &preview_accounting,
                shutdown: phase.shutdown,
                interrupted: phase.interrupted,
            };
            query_phase
                .run(
                    &self.sql,
                    async { crate::read_result_tracked(client, &sql, self.limit, false, &progress).await.map(Ok) },
                    limits,
                    &progress,
                    culprit_read::RetentionControl::default(),
                )
                .await?
        };
        phase.setup(client, "ROLLBACK".into()).await?;
        Ok(result)
    }
}

async fn initialize(
    client: &mut Conn,
    connector: &Connector,
    scope: Scope,
    require_timeout: bool,
    phase: &mut Phase<'_>,
) -> Result<()> {
    let rows = phase
        .rows(
            client,
            &Request {
                sql: "SELECT VERSION() AS version".into(),
                parameters: vec![],
                row_limit: 1,
                byte_limit: 1024,
                prepared: false,
            },
            RowOutput::buffered(),
        )
        .await?;
    let version = rows
        .first()
        .ok_or_else(|| CollectionStop::InvalidPage.error())?
        .native_text("version")?
        .ok_or_else(|| CollectionStop::InvalidPage.error())?;
    let flavor = Flavor::of_version(&version);
    connector.record_version(version);
    // Do not inherit a server/application default that could retain metadata locks across polls.
    phase.setup(client, "SET SESSION autocommit = 1".into()).await?;
    for (statement, _) in SESSION_SETUP {
        phase.setup(client, statement.into()).await?;
    }
    let setting = flavor.timeout_for(Duration::from_millis(phase.accounting.budget.statement_timeout_ms.into()));
    if require_timeout {
        phase.setup(client, setting).await?;
    } else {
        phase.optional_setup(client, setting).await?;
    }
    if let (Scope::Database, Some(database)) = (scope, &connector.database) {
        phase.setup(client, format!("USE `{}`", database.replace('`', "``"))).await?;
    }
    Ok(())
}

pub(super) async fn drain<P: Protocol>(
    mut result: QueryResult<'_, '_, P>,
    request: &Request,
    progress: &ReadProgress,
    limits: mpsc::Sender<CollectionStop>,
    mut output: RowOutput<Rec>,
) -> mysql_async::Result<Result<Vec<Rec>>> {
    let mut budget = culprit_read::ReadBudget::new(request.row_limit, request.byte_limit);
    let mut failure = None;
    while let Some(row) = result.next().await? {
        if failure.is_some() || output.discarding() {
            continue;
        }
        let retained = match Rec::from_native(row) {
            Ok(row) => {
                let bytes = row.decoded_value_bytes();
                progress.rows.fetch_add(1, Ordering::Relaxed);
                progress.bytes.fetch_add(bytes as u64, Ordering::Relaxed);
                match budget.row_started().and_then(|_| budget.charge_values(bytes)) {
                    Ok(()) => output.push(row, bytes).await,
                    Err(error) => Err(error),
                }
            }
            Err(error) => Err(error),
        };
        if let Err(error) = retained {
            let _ = limits.try_send(CollectionStop::from_error(&error));
            output.discard();
            failure = Some(error);
        }
    }
    if !result.is_empty() {
        let _ = limits.try_send(CollectionStop::InvalidPage);
        failure = Some(CollectionStop::InvalidPage.error());
    }
    result.drop_result().await?;
    progress.drained.store(true, Ordering::Release);
    Ok(match failure {
        Some(error) => Err(error),
        None => Ok(output.finish().await),
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn request_markers_are_unique_and_remain_one_tagged_sql_comment_prefix() {
        let first = super::marker();
        let second = super::marker();
        assert_ne!(first, second);
        assert!(first.starts_with(crate::connect::TAG));
        assert!(first.len() < 100);
        assert!(culprit_core::sql::reads_only(&format!("{first} SELECT 1"), culprit_core::Engine::Mysql));
    }
}
