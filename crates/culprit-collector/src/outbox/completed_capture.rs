//! One bounded canonical verification pass owns local lookup, file reads and provisional row retention.
use super::{Files, OutboxDelivery, artifacts, problem, valid_id};
use crate::capture::CollectionCancellation;
use culprit_core::{Error, ErrorDetail, Result, limits::OperationHandle, telemetry::*};
use std::time::Duration;
use tokio::time::Instant;

const DEFAULT_ROWS: u32 = 10_000;
const DEFAULT_BYTES: u64 = 8 * 1024 * 1024;
pub(super) const DEFAULT_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_BYTES: u64 = 256 * 1024 * 1024;
const MAX_TIMEOUT: Duration = Duration::from_secs(300);

/// Independent storage-read allowance. These limits never change native collection or cost qualification.
#[derive(Clone, Copy, Debug)]
pub struct CompletedCaptureBudget {
    pub max_rows: u32,
    /// Actual manifest/artifact bytes verified, including streams not retained by this analysis.
    pub max_bytes: u64,
    pub timeout: Duration,
}
impl Default for CompletedCaptureBudget {
    fn default() -> Self {
        Self { max_rows: DEFAULT_ROWS, max_bytes: DEFAULT_BYTES, timeout: DEFAULT_TIMEOUT }
    }
}
impl CompletedCaptureBudget {
    pub fn validate(self) -> Result<Self> {
        if !(1..=artifacts::MAX_CANONICAL_ROWS).contains(&self.max_rows)
            || !(1..=MAX_BYTES).contains(&self.max_bytes)
            || self.timeout.is_zero()
            || self.timeout > MAX_TIMEOUT
        {
            return Err(DeliveryProblem::Capacity.error());
        }
        Ok(self)
    }
}

pub(super) enum ReadControl {
    Collection { cancellation: CollectionCancellation, deadline: Instant },
    Operation { operation: OperationHandle, deadline: Instant },
}
impl ReadControl {
    pub fn check(&self) -> Result<()> {
        match self {
            Self::Collection { cancellation, .. } if cancellation.is_cancelled() => {
                Err(CollectionStop::Cancelled.error())
            }
            Self::Collection { deadline, .. } if Instant::now() >= *deadline => Err(CollectionStop::TimedOut.error()),
            Self::Operation { operation, .. } if operation.is_cancelled() => Err(CollectionStop::Cancelled.error()),
            Self::Operation { operation, deadline }
                if operation.remaining().is_zero() || Instant::now() >= *deadline =>
            {
                Err(CollectionStop::TimedOut.error())
            }
            _ => Ok(()),
        }
    }

    async fn stopped(&self) -> Error {
        match self {
            Self::Collection { cancellation, deadline } => tokio::select! {
                biased;
                _ = cancellation.cancelled() => CollectionStop::Cancelled.error(),
                _ = tokio::time::sleep_until(*deadline) => CollectionStop::TimedOut.error(),
            },
            Self::Operation { deadline, .. } => {
                tokio::time::sleep_until(*deadline).await;
                self.check().err().unwrap_or_else(|| CollectionStop::TimedOut.error())
            }
        }
    }
}

pub(super) enum ReadRequest {
    Snapshot { attempt: DiagnosticAttemptReceipt, dataset: SnapshotDataset },
    QueryHistory { connection: String, observation_id: String },
}
impl ReadRequest {
    fn observation_id(&self) -> &str {
        match self {
            Self::Snapshot { attempt, .. } => &attempt.capture.observation_id,
            Self::QueryHistory { observation_id, .. } => observation_id,
        }
    }

    fn retains(&self, stream: TelemetryStream) -> bool {
        match self {
            Self::Snapshot { dataset, .. } => stream == dataset.stream(),
            Self::QueryHistory { .. } => {
                matches!(stream, TelemetryStream::QueryRuntime | TelemetryStream::QueryDefinitions)
            }
        }
    }
}

pub(super) struct CompletedCaptureRead {
    pub request: ReadRequest,
    pub control: ReadControl,
    pub budget: CompletedCaptureBudget,
    pub rows: artifacts::CanonicalRows,
    pub bytes_read: u64,
}
impl CompletedCaptureRead {
    pub fn snapshot(
        attempt: DiagnosticAttemptReceipt,
        dataset: SnapshotDataset,
        budget: CompletedCaptureBudget,
        cancellation: CollectionCancellation,
    ) -> Result<Self> {
        let budget = budget.validate()?;
        if !attempt.is_valid() || attempt.manifest_sha256.is_none() {
            return Err(DeliveryProblem::InvalidContract.error());
        }
        let deadline = Instant::now().checked_add(budget.timeout).ok_or_else(|| CollectionStop::TimedOut.error())?;
        Ok(Self::new(
            ReadRequest::Snapshot { attempt, dataset },
            ReadControl::Collection { cancellation, deadline },
            budget,
        ))
    }

    pub fn query_history(
        connection: &str,
        observation_id: &str,
        budget: CompletedCaptureBudget,
        operation: OperationHandle,
    ) -> Result<Self> {
        let budget = budget.validate()?;
        if !valid_id(observation_id) || connection.is_empty() || connection.len() > 512 {
            return Err(DeliveryProblem::InvalidContract.error());
        }
        let deadline = Instant::now()
            .checked_add(budget.timeout.min(operation.remaining()))
            .ok_or_else(|| CollectionStop::TimedOut.error())?;
        Ok(Self::new(
            ReadRequest::QueryHistory { connection: connection.into(), observation_id: observation_id.into() },
            ReadControl::Operation { operation, deadline },
            budget,
        ))
    }

    fn new(request: ReadRequest, control: ReadControl, budget: CompletedCaptureBudget) -> Self {
        Self { request, control, budget, rows: artifacts::CanonicalRows::new(budget.max_rows), bytes_read: 0 }
    }
}
impl artifacts::VerificationObserver for CompletedCaptureRead {
    fn check(&self) -> Result<()> {
        self.control.check()
    }
    fn caller_refused(&self, error: &Error) -> bool {
        matches!(
            error.detail,
            Some(ErrorDetail::Delivery(
                DeliveryProblem::Capacity | DeliveryProblem::Conflict | DeliveryProblem::UnsupportedSchema
            )) | Some(ErrorDetail::CollectionStopped(CollectionStop::Cancelled | CollectionStop::TimedOut))
        )
    }
    fn allowance(&self, requested: usize) -> Result<usize> {
        self.check()?;
        let remaining = self.budget.max_bytes.checked_sub(self.bytes_read).ok_or_else(capacity)?;
        Ok(requested.min(usize::try_from(remaining).unwrap_or(usize::MAX)))
    }
    fn charged(&mut self, bytes: usize) -> Result<()> {
        self.bytes_read = self.bytes_read.checked_add(bytes as u64).ok_or_else(capacity)?;
        self.check()
    }
    fn manifest(&mut self, manifest: &CollectionManifest) -> Result<()> {
        self.check()?;
        let source = manifest.description.as_ref().ok_or_else(unsupported)?;
        let rows = match &self.request {
            ReadRequest::Snapshot { dataset, .. } => {
                source
                    .observation
                    .snapshot()
                    .filter(|header| header.dataset == *dataset)
                    .ok_or_else(unsupported)?
                    .records
            }
            ReadRequest::QueryHistory { .. } if source.query_history().is_some() => manifest
                .files
                .iter()
                .filter(|file| self.request.retains(file.stream))
                .try_fold(0u32, |sum, file| sum.checked_add(file.rows).ok_or_else(capacity))?,
            _ => return Err(unsupported()),
        };
        if rows > self.budget.max_rows {
            return Err(capacity());
        }
        Ok(())
    }
    fn row(&mut self, row: &ExportedRecord, ordinal: u32) -> Result<()> {
        self.check()?;
        if self.request.retains(row.record.stream()) && !self.rows.retain(row, ordinal) {
            return Err(capacity());
        }
        Ok(())
    }
}
use artifacts::VerificationObserver;

/// Provisional rows leave the worker only after every file hash and the journal receipt validate.
#[derive(Debug)]
pub(super) struct CompletedCapture {
    pub source: CollectionDescription,
    pub capture: DiagnosticCaptureRef,
    pub records: Vec<ExportedRecord>,
    pub row_ordinals: Vec<u32>,
    pub streams: Vec<CapturedStreamRows>,
    pub references: ReferenceCoverage,
    pub collected_from: String,
    pub collected_until: String,
    pub bytes_read: u64,
}

impl OutboxDelivery {
    pub(super) async fn with_completed_capture<T: Send + 'static>(
        &self,
        mut read: CompletedCaptureRead,
        project: impl FnOnce(CompletedCapture) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        read.check()?;
        let access = tokio::select! {
            biased;
            error = read.control.stopped() => return Err(error),
            access = self.acquire_files() => access,
        };
        // Do not wrap started work in timeout/abort: the same worker keeps Files until it has stopped.
        access
            .run(move |files| {
                let completed = files.completed_capture(&mut read)?;
                let result = project(completed)?;
                read.check()?;
                Ok(result)
            })
            .await
    }
}
impl Files {
    fn completed_capture(&mut self, read: &mut CompletedCaptureRead) -> Result<CompletedCapture> {
        read.check()?;
        let capture = self.journal.capture(read.request.observation_id())?;
        match &read.request {
            ReadRequest::Snapshot { attempt, .. } if capture.identity != attempt.capture => {
                return Err(DeliveryProblem::Conflict.error());
            }
            ReadRequest::QueryHistory { connection, .. } if capture.connection != *connection => {
                return Err(problem(OutboxProblem::MissingCapture));
            }
            _ => (),
        }
        if capture.state != OutboxCaptureState::Ready || capture.source_work != SourceWorkState::Quiescent {
            return Err(problem(OutboxProblem::SourceUnconfirmed));
        }
        let validated = self.validated_observed(&capture.identity.observation_id, read)?;
        read.check()?;
        if let ReadRequest::Snapshot { attempt, .. } = &read.request
            && attempt.manifest_sha256.as_deref() != Some(&validated.manifest_sha)
        {
            return Err(DeliveryProblem::Conflict.error());
        }
        let manifest = validated.manifest;
        let source = manifest.description.ok_or_else(integrity)?;
        let capture = DiagnosticCaptureRef {
            capture: manifest.capture,
            manifest_sha256: validated.manifest_sha,
            source_scope_id: source.captured_range()?.scope_id,
        };
        if !capture.is_valid() {
            return Err(integrity());
        }
        let streams = manifest
            .files
            .iter()
            .filter(|file| read.request.retains(file.stream))
            .map(|file| CapturedStreamRows { stream: file.stream, rows: file.rows })
            .collect();
        let (records, row_ordinals) = read.rows.drain();
        read.check()?;
        Ok(CompletedCapture {
            source,
            capture,
            records,
            row_ordinals,
            streams,
            references: manifest.references,
            collected_from: manifest.started_at,
            collected_until: manifest.finished_at.ok_or_else(integrity)?,
            bytes_read: read.bytes_read,
        })
    }
}

fn capacity() -> Error {
    DeliveryProblem::Capacity.error()
}
fn unsupported() -> Error {
    DeliveryProblem::UnsupportedSchema.error()
}
fn integrity() -> Error {
    problem(OutboxProblem::Integrity)
}
