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

/// One explicitly delegated local identity pair. The owning worker checks the original journal; this value
/// establishes neither source ownership nor a database/member correlation by itself.
#[derive(Clone, Copy, Debug)]
pub struct CompletedCaptureLookup<'a> {
    pub connection: &'a str,
    pub observation_id: &'a str,
}

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

    pub(super) async fn stopped(&self) -> Error {
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
    SnapshotLookup { connection: String, observation_id: String, datasets: Vec<SnapshotDataset> },
    QueryHistory { connection: String, observation_id: String },
    ActivityLookup { connection: String, observation_id: String },
    ActivityReference { connection: String, reference: DiagnosticCaptureRef },
}
impl ReadRequest {
    fn observation_id(&self) -> &str {
        match self {
            Self::Snapshot { attempt, .. } => &attempt.capture.observation_id,
            Self::SnapshotLookup { observation_id, .. }
            | Self::QueryHistory { observation_id, .. }
            | Self::ActivityLookup { observation_id, .. } => observation_id,
            Self::ActivityReference { reference, .. } => &reference.capture.observation_id,
        }
    }

    fn retains(&self, stream: TelemetryStream) -> bool {
        match self {
            Self::Snapshot { dataset, .. } => stream == dataset.stream(),
            Self::SnapshotLookup { datasets, .. } => datasets.iter().any(|dataset| stream == dataset.stream()),
            Self::QueryHistory { .. } => {
                matches!(stream, TelemetryStream::QueryRuntime | TelemetryStream::QueryDefinitions)
            }
            Self::ActivityLookup { .. } | Self::ActivityReference { .. } => {
                stream == TelemetryStream::ActivityObservations
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
    selected_rows: u32,
    required_connection: Option<String>,
}
impl CompletedCaptureRead {
    pub fn activity_lookup(
        lookup: CompletedCaptureLookup<'_>,
        budget: CompletedCaptureBudget,
        operation: OperationHandle,
    ) -> Result<Self> {
        Self::lookup(
            ReadRequest::ActivityLookup {
                connection: lookup.connection.into(),
                observation_id: lookup.observation_id.into(),
            },
            lookup,
            budget,
            operation,
        )
    }

    /// Select the exact prerequisite without replacing the original deadline or cumulative row/byte counters.
    pub fn select_activity_reference(&mut self, connection: &str, reference: DiagnosticCaptureRef) -> Result<()> {
        self.check()?;
        validate_connection(connection)?;
        if !reference.is_valid() || !valid_id(&reference.capture.observation_id) {
            return Err(DeliveryProblem::InvalidContract.error());
        }
        self.request = ReadRequest::ActivityReference { connection: connection.into(), reference };
        Ok(())
    }
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
        lookup: CompletedCaptureLookup<'_>,
        budget: CompletedCaptureBudget,
        operation: OperationHandle,
    ) -> Result<Self> {
        Self::lookup(
            ReadRequest::QueryHistory {
                connection: lookup.connection.into(),
                observation_id: lookup.observation_id.into(),
            },
            lookup,
            budget,
            operation,
        )
    }

    pub fn snapshot_lookup(
        lookup: CompletedCaptureLookup<'_>,
        datasets: &[SnapshotDataset],
        budget: CompletedCaptureBudget,
        operation: OperationHandle,
    ) -> Result<Self> {
        let Some(first) = datasets.first() else { return Err(DeliveryProblem::InvalidContract.error()) };
        if datasets
            .iter()
            .enumerate()
            .any(|(index, dataset)| dataset.streams() != first.streams() || datasets[..index].contains(dataset))
        {
            return Err(DeliveryProblem::InvalidContract.error());
        }
        Self::lookup(
            ReadRequest::SnapshotLookup {
                connection: lookup.connection.into(),
                observation_id: lookup.observation_id.into(),
                datasets: datasets.to_vec(),
            },
            lookup,
            budget,
            operation,
        )
    }

    pub fn for_connection(mut self, connection: &str) -> Result<Self> {
        validate_connection(connection)?;
        self.required_connection = Some(connection.into());
        Ok(self)
    }

    fn lookup(
        request: ReadRequest,
        lookup: CompletedCaptureLookup<'_>,
        budget: CompletedCaptureBudget,
        operation: OperationHandle,
    ) -> Result<Self> {
        let budget = budget.validate()?;
        validate_connection(lookup.connection)?;
        if !valid_id(lookup.observation_id) {
            return Err(DeliveryProblem::InvalidContract.error());
        }
        let deadline = Instant::now()
            .checked_add(budget.timeout.min(operation.remaining()))
            .ok_or_else(|| CollectionStop::TimedOut.error())?;
        Ok(Self::new(request, ReadControl::Operation { operation, deadline }, budget))
    }

    fn new(request: ReadRequest, control: ReadControl, budget: CompletedCaptureBudget) -> Self {
        Self {
            request,
            control,
            budget,
            rows: artifacts::CanonicalRows::new(budget.max_rows),
            bytes_read: 0,
            selected_rows: 0,
            required_connection: None,
        }
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
            ReadRequest::SnapshotLookup { datasets, .. } => {
                source
                    .observation
                    .snapshot()
                    .filter(|header| datasets.contains(&header.dataset))
                    .ok_or_else(unsupported)?
                    .records
            }
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
            ReadRequest::ActivityLookup { .. } | ReadRequest::ActivityReference { .. } => {
                source.observation.activity().filter(|frame| frame.is_valid()).ok_or_else(unsupported)?.observations
            }
            _ => return Err(unsupported()),
        };
        self.selected_rows = self.selected_rows.checked_add(rows).ok_or_else(capacity)?;
        if self.selected_rows > self.budget.max_rows {
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
    pub connection: String,
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
        read: CompletedCaptureRead,
        worker_guard: impl Send + 'static,
        project: impl FnOnce(CompletedCapture) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        self.with_capture_worker(read, worker_guard, move |files, read| project(files.completed_capture(read)?)).await
    }

    /// One worker owns every requested original and its shared read allowance through projection and settlement.
    pub(super) async fn with_capture_worker<T: Send + 'static>(
        &self,
        mut read: CompletedCaptureRead,
        worker_guard: impl Send + 'static,
        project: impl FnOnce(&mut Files, &mut CompletedCaptureRead) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        read.check()?;
        let access = tokio::select! {
            biased;
            error = read.control.stopped() => return Err(error),
            access = self.acquire_files() => access,
        };
        // Do not wrap started work in timeout/abort: the same worker keeps Files until it has stopped.
        access
            .run_owned(worker_guard, move |files| {
                let result = project(files, &mut read)?;
                read.check()?;
                Ok(result)
            })
            .await
    }
}
impl Files {
    pub(super) fn completed_capture(&mut self, read: &mut CompletedCaptureRead) -> Result<CompletedCapture> {
        read.check()?;
        let started_bytes = read.bytes_read;
        let capture = self.journal.capture(read.request.observation_id())?;
        if read.required_connection.as_ref().is_some_and(|connection| capture.connection != *connection) {
            return Err(problem(OutboxProblem::MissingCapture));
        }
        match &read.request {
            ReadRequest::Snapshot { attempt, .. } if capture.identity != attempt.capture => {
                return Err(DeliveryProblem::Conflict.error());
            }
            ReadRequest::ActivityReference { reference, .. } if capture.identity != reference.capture => {
                return Err(DeliveryProblem::Conflict.error());
            }
            ReadRequest::SnapshotLookup { connection, .. }
            | ReadRequest::QueryHistory { connection, .. }
            | ReadRequest::ActivityLookup { connection, .. }
            | ReadRequest::ActivityReference { connection, .. }
                if capture.connection != *connection =>
            {
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
        let connection = capture.connection;
        let capture = DiagnosticCaptureRef {
            capture: manifest.capture,
            manifest_sha256: validated.manifest_sha,
            source_scope_id: source.captured_range()?.scope_id,
        };
        if !capture.is_valid() {
            return Err(integrity());
        }
        if let ReadRequest::ActivityReference { reference, .. } = &read.request
            && reference != &capture
        {
            return Err(DeliveryProblem::Conflict.error());
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
            connection,
            source,
            capture,
            records,
            row_ordinals,
            streams,
            references: manifest.references,
            collected_from: manifest.started_at,
            collected_until: manifest.finished_at.ok_or_else(integrity)?,
            bytes_read: read.bytes_read.checked_sub(started_bytes).ok_or_else(integrity)?,
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

fn validate_connection(connection: &str) -> Result<()> {
    if connection.is_empty() || connection.len() > 512 {
        return Err(DeliveryProblem::InvalidContract.error());
    }
    Ok(())
}
