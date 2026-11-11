use super::*;
use crate::{connect::map_error, rows::monitoring};
use culprit_core::telemetry::{ReadProgress, ReadTaskOutcome};
use futures_util::StreamExt;
use tokio::sync::{mpsc, oneshot};
use tokio_postgres::SimpleQueryMessage;

const CANCELLATION_ACK_TIMEOUT: Duration = Duration::from_secs(5);
struct TaskLifetime {
    fence: culprit_read::ReadFence,
    settled: bool,
}
impl Drop for TaskLifetime {
    fn drop(&mut self) {
        if !self.settled {
            self.fence.mark_uncertain();
        }
    }
}

pub(super) struct ReadRequest {
    pub sql: String,
    pub row_limit: usize,
    pub byte_limit: usize,
    pub timeout: Duration,
}
pub(super) struct ReadTask {
    pub client: Client,
    pub connector: Connector,
    pub request: ReadRequest,
    pub metrics: ReadMetricsRecorder,
    pub template: Option<String>,
    pub fence: culprit_read::ReadFence,
}
#[derive(Default)]
struct QueryProgress {
    counters: ReadProgress,
    started: AtomicBool,
}
struct TaskResult {
    result: Result<Vec<Row>>,
    confirmed: bool,
    interrupted: bool,
    outcome: ReadTaskOutcome,
}
impl ReadTask {
    // Kept outside the source future: cleanup continues when the controller retires that future.
    pub(super) async fn run(self, output: RowOutput<Row>, cancellation: oneshot::Receiver<()>) -> CompletedRead {
        let mut lifetime = TaskLifetime { fence: self.fence.clone(), settled: false };
        let progress = QueryProgress::default();
        let started = std::time::Instant::now();
        let settled = self.settle(output, &progress, cancellation).await;
        self.metrics.finish(self.template.as_deref(), started.elapsed(), &progress.counters, &settled.outcome);
        lifetime.settled = settled.confirmed;
        // A delayed CancelRequest must never race a later statement on a reused session.
        let client = (!settled.interrupted && settled.confirmed).then_some(self.client);
        CompletedRead { client, result: settled.result, confirmed: settled.confirmed }
    }
    async fn settle(
        &self,
        output: RowOutput<Row>,
        progress: &QueryProgress,
        cancellation: oneshot::Receiver<()>,
    ) -> TaskResult {
        let (limits, mut limit_reached) = mpsc::channel(1);
        let query = drain(&self.client, &self.request, progress, limits, output);
        tokio::pin!(query);
        let reason = tokio::select! {
            biased;
            _ = cancellation => CollectionStop::Cancelled,
            _ = tokio::time::sleep(self.request.timeout) => CollectionStop::TimedOut,
            Some(reason) = limit_reached.recv() => reason,
            result = &mut query => {
                let confirmed = progress.counters.drained.load(Ordering::Acquire);
                let timed_out = result.as_ref().err().is_some_and(|error| error.code == culprit_core::ErrorCode::Timeout);
                let outcome = ReadTaskOutcome { failed: result.is_err(), timed_out, unconfirmed: !confirmed, ..Default::default() };
                return TaskResult { result, confirmed, interrupted: false, outcome };
            }
        };
        let confirmed = if !progress.started.load(Ordering::Acquire) {
            true
        } else {
            // CancelRequest has no acknowledgement. Only ReadyForQuery on this exact response proves shutdown.
            tokio::time::timeout(CANCELLATION_ACK_TIMEOUT, async {
                let token = self.client.cancel_token();
                let cancel = self.connector.cancel(&token);
                tokio::pin!(cancel);
                tokio::select! {
                    _ = &mut query => (),
                    _ = &mut cancel => { let _ = (&mut query).await; }
                }
                progress.counters.drained.load(Ordering::Acquire)
            })
            .await
            .unwrap_or(false)
        };
        let outcome = ReadTaskOutcome {
            failed: true,
            timed_out: reason == CollectionStop::TimedOut,
            cancelled: reason == CollectionStop::Cancelled,
            unconfirmed: !confirmed,
        };
        TaskResult {
            result: Err(if confirmed { reason.error() } else { CollectionStop::CancellationUnconfirmed.error() }),
            confirmed,
            interrupted: true,
            outcome,
        }
    }
}

async fn drain(
    client: &Client,
    request: &ReadRequest,
    progress: &QueryProgress,
    limits: mpsc::Sender<CollectionStop>,
    mut output: RowOutput<Row>,
) -> Result<Vec<Row>> {
    progress.started.store(true, Ordering::Release);
    let stream = client.simple_query_raw(&monitoring(&request.sql)).await.map_err(map_error)?;
    tokio::pin!(stream);
    let mut budget = culprit_read::ReadBudget::new(request.row_limit, request.byte_limit);
    let mut failure = None;
    let mut sets = 0;
    while let Some(message) = stream.next().await {
        match message {
            Ok(SimpleQueryMessage::RowDescription(_)) => {
                sets += 1;
                if sets > 1 && failure.is_none() {
                    failure = Some(CollectionStop::InvalidPage.error());
                    let _ = limits.try_send(CollectionStop::InvalidPage);
                }
            }
            Ok(SimpleQueryMessage::Row(row)) => {
                if failure.is_some() || output.discarding() {
                    continue;
                }
                let bytes = (0..row.len())
                    .fold(0usize, |total, index| total.saturating_add(row.get(index).map_or(0, str::len)));
                progress.counters.rows.fetch_add(1, Ordering::Relaxed);
                progress.counters.bytes.fetch_add(bytes as u64, Ordering::Relaxed);
                let retained = match budget.row_started().and_then(|_| budget.charge_values(bytes)) {
                    Ok(()) => output.push(Row::from_native(row), bytes).await,
                    Err(error) => Err(error),
                };
                if let Err(error) = retained {
                    let _ = limits.try_send(CollectionStop::from_error(&error));
                    output.discard();
                    failure = Some(error);
                }
            }
            Ok(_) => (),
            Err(error) => {
                // A PostgreSQL ErrorResponse is followed by ReadyForQuery. Continue polling after that error.
                // Transport/decoding errors have no such completion proof and must not be treated as quiescent.
                let database_error = error.as_db_error().is_some();
                let mapped = map_error(error);
                if !database_error {
                    return Err(mapped);
                }
                if failure.is_none() {
                    failure = Some(mapped);
                }
            }
        }
    }
    progress.counters.drained.store(true, Ordering::Release);
    match failure {
        Some(error) => Err(error),
        None => Ok(output.finish().await),
    }
}
