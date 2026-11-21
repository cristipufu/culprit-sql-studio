use super::response::{ReadOutput, ReadRow, ReadRows};
use super::*;
use crate::rows::monitoring;
use culprit_core::telemetry::{ReadProgress, ReadTaskOutcome};
use futures_util::StreamExt;
use tokio::sync::{mpsc, oneshot};
use tokio_postgres::SimpleQueryMessage;

const CANCELLATION_ACK_TIMEOUT: Duration = Duration::from_secs(5);
pub(super) enum ReadDispatch {
    Simple,
    TextArray(Vec<String>),
}
pub(super) struct ReadRequest {
    pub sql: String,
    pub row_limit: usize,
    pub byte_limit: usize,
    pub timeout: Duration,
    pub dispatch: ReadDispatch,
    pub profile: Option<culprit_read::SourceProfileGuard>,
}
pub(super) struct ReadTask {
    pub client: Client,
    pub connector: Connector,
    pub request: ReadRequest,
    pub metrics: ReadMetricsRecorder,
    pub template: Option<String>,
    pub fence: culprit_read::ReadFence,
    pub connection_phase: Option<culprit_core::telemetry::ReadConnectionPhaseGuard>,
}
#[derive(Default)]
struct QueryProgress {
    counters: ReadProgress,
    started: AtomicBool,
}
struct TaskResult {
    result: NativeResult<ReadRows>,
    confirmed: bool,
    interrupted: bool,
    outcome: ReadTaskOutcome,
}
impl ReadTask {
    // Kept outside the source future: cleanup continues when the controller retires that future.
    pub(super) async fn run(mut self, output: ReadOutput, cancellation: oneshot::Receiver<()>) -> CompletedRead {
        let lifetime = match self.fence.start_work() {
            Ok(lifetime) => lifetime,
            Err(error) => return CompletedRead { client: None, result: Err(error.into()), confirmed: false },
        };
        let progress = QueryProgress::default();
        let started = std::time::Instant::now();
        let settled = self.settle(output, &progress, cancellation).await;
        self.metrics.finish(self.template.as_deref(), started.elapsed(), &progress.counters, &settled.outcome);
        if let Some(phase) = self.connection_phase.take() {
            let outcome = if !settled.confirmed {
                culprit_core::telemetry::ReadConnectionOutcome::Unconfirmed
            } else {
                match &settled.result {
                    Ok(_) => culprit_core::telemetry::ReadConnectionOutcome::Completed,
                    Err(error) => culprit_core::telemetry::ReadConnectionOutcome::from_error(&error.error),
                }
            };
            phase.finish(outcome);
        }
        lifetime.complete(if settled.confirmed { SourceShutdown::Quiescent } else { SourceShutdown::Unconfirmed });
        // A delayed CancelRequest must never race a later statement on a reused session.
        let client = (!settled.interrupted && settled.confirmed).then_some(self.client);
        CompletedRead { client, result: settled.result, confirmed: settled.confirmed }
    }
    async fn settle(
        &self,
        output: ReadOutput,
        progress: &QueryProgress,
        cancellation: oneshot::Receiver<()>,
    ) -> TaskResult {
        if let Some(profile) = &self.request.profile
            && let Err(error) = profile.ensure_active()
        {
            // No driver call has started. Keep the original client for its existing close owner.
            return TaskResult {
                result: Err(error.into()),
                confirmed: true,
                interrupted: false,
                outcome: ReadTaskOutcome::stopped(CollectionStop::SourceChanged),
            };
        }
        let retention = output.retention_control();
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
                let timed_out = result.as_ref().err().is_some_and(|error| error.error.code == culprit_core::ErrorCode::Timeout);
                // A limit can be signalled during this same final poll, before a later protocol error returns.
                let mut outcome = limit_reached.try_recv().ok().map(ReadTaskOutcome::stopped).unwrap_or_default();
                outcome.failed = result.is_err();
                outcome.timed_out |= timed_out;
                outcome.unconfirmed = !confirmed;
                return TaskResult { result, confirmed, interrupted: false, outcome };
            }
        };
        retention.discard();
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
        let outcome = ReadTaskOutcome { unconfirmed: !confirmed, ..ReadTaskOutcome::stopped(reason) };
        TaskResult {
            result: Err(if confirmed { reason.error() } else { CollectionStop::CancellationUnconfirmed.error() }.into()),
            confirmed,
            interrupted: true,
            outcome,
        }
    }
}

enum Message {
    Description,
    Row(ReadRow),
    Other,
}
async fn drain(
    client: &Client,
    request: &ReadRequest,
    progress: &QueryProgress,
    limits: mpsc::Sender<CollectionStop>,
    output: ReadOutput,
) -> NativeResult<ReadRows> {
    progress.started.store(true, Ordering::Release);
    let sql = monitoring(&request.sql);
    match &request.dispatch {
        ReadDispatch::Simple => {
            let stream = client.simple_query_raw(&sql).await.map_err(NativeReadFailure::from)?;
            let stream = stream.map(|message| {
                message.map(|message| match message {
                    SimpleQueryMessage::RowDescription(_) => Message::Description,
                    SimpleQueryMessage::Row(row) => Message::Row(ReadRow::Text(row)),
                    _ => Message::Other,
                })
            });
            drain_messages(stream, request, progress, limits, output).await
        }
        ReadDispatch::TextArray(values) => {
            let parameters = [values as &(dyn tokio_postgres::types::ToSql + Sync)];
            let stream = client.query_raw(sql.as_str(), parameters).await.map_err(NativeReadFailure::from)?;
            let stream = stream.map(|row| row.map(|row| Message::Row(ReadRow::Native(row))));
            drain_messages(stream, request, progress, limits, output).await
        }
    }
}
async fn drain_messages(
    stream: impl futures_util::Stream<Item = std::result::Result<Message, tokio_postgres::Error>>,
    request: &ReadRequest,
    progress: &QueryProgress,
    limits: mpsc::Sender<CollectionStop>,
    mut output: ReadOutput,
) -> NativeResult<ReadRows> {
    tokio::pin!(stream);
    let mut budget = culprit_read::ReadBudget::new(request.row_limit, request.byte_limit);
    let mut failure: Option<NativeReadFailure> = None;
    let mut sets = 0;
    while let Some(message) = stream.next().await {
        match message {
            Ok(Message::Description) => {
                sets += 1;
                if sets > 1 && failure.is_none() {
                    failure = Some(CollectionStop::InvalidPage.error().into());
                    let _ = limits.try_send(CollectionStop::InvalidPage);
                }
            }
            Ok(Message::Row(row)) => {
                if failure.is_some() || output.discarding() {
                    continue;
                }
                let bytes = match row.value_bytes() {
                    Ok(bytes) => bytes,
                    Err(error) => {
                        let _ = limits.try_send(CollectionStop::from_error(&error));
                        output.discard();
                        failure = Some(error.into());
                        continue;
                    }
                };
                progress.counters.rows.fetch_add(1, Ordering::Relaxed);
                progress.counters.bytes.fetch_add(bytes as u64, Ordering::Relaxed);
                let retained = match budget.row_started().and_then(|_| budget.charge_values(bytes)) {
                    Ok(()) => output.push(row, bytes).await,
                    Err(error) => Err(error),
                };
                if let Err(error) = retained {
                    let _ = limits.try_send(CollectionStop::from_error(&error));
                    output.discard();
                    failure = Some(error.into());
                }
            }
            Ok(Message::Other) => (),
            Err(error) => {
                // Both native protocols return ErrorResponse before ReadyForQuery. Preserve the exact original
                // SQLSTATE while draining; a transport error has no such completion proof.
                let database_error = error.as_db_error().is_some();
                let error = NativeReadFailure::from(error);
                if !database_error {
                    return Err(error);
                }
                if failure.is_none() {
                    failure = Some(error);
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
