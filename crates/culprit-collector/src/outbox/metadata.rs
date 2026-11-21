//! Connection-bound metadata inspection reuses the bounded manifest parser and existing worker owner.
use super::{
    Files, Outbox, OutboxDelivery,
    artifacts::VerificationObserver,
    attempts,
    completed_capture::{CompletedCaptureBudget, CompletedCaptureLookup, CompletedCaptureRead, ReadRequest},
    journal, problem, read_identity,
};
use culprit_core::{Result, limits::OperationHandle, telemetry::*};
use std::path::Path;

/// Only the original journal/manifest owner may construct this value. It carries no native result rows.
#[derive(Debug)]
pub struct InspectedCaptureManifest {
    connection: String,
    manifest: CollectionManifest,
    sha256: String,
    sealed: bool,
    bytes_read: u64,
}
impl InspectedCaptureManifest {
    pub fn connection(&self) -> &str {
        &self.connection
    }
    pub fn manifest(&self) -> &CollectionManifest {
        &self.manifest
    }
    pub fn sha256(&self) -> &str {
        &self.sha256
    }
    pub fn is_sealed(&self) -> bool {
        self.sealed
    }
    pub fn bytes_read(&self) -> u64 {
        self.bytes_read
    }
}
impl Outbox {
    /// No exclusive capture lock, native payload or credentials. Started work retains the host's gate.
    pub async fn with_inspected_manifest<T: Send + 'static>(
        directory: &Path,
        lookup: CompletedCaptureLookup<'_>,
        budget: CompletedCaptureBudget,
        operation: OperationHandle,
        worker_guard: impl Send + 'static,
        project: impl FnOnce(Option<InspectedCaptureManifest>) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let mut read = CompletedCaptureRead::source_overhead(lookup, budget, operation)?;
        read.check()?;
        let directory = directory.to_owned();
        let language = culprit_core::i18n::current();
        tokio::task::spawn_blocking(move || {
            let _guard = worker_guard;
            culprit_core::i18n::scope(&language.code, || {
                read.check()?;
                let identity = read_identity(&directory)?;
                let journal = journal::Journal::open(&directory.join("journal.sqlite3"), &identity.id, false)?;
                let original = inspect(&directory, &journal, &mut read)?;
                let value = project(original)?;
                read.check()?;
                Ok(value)
            })
        })
        .await
        .map_err(|_| problem(OutboxProblem::Storage))?
    }
}
impl OutboxDelivery {
    /// The same serialized Files worker performs original lookup, bounded read and postcheck through projection.
    pub async fn with_inspected_manifest<T: Send + 'static>(
        &self,
        lookup: CompletedCaptureLookup<'_>,
        budget: CompletedCaptureBudget,
        operation: OperationHandle,
        worker_guard: impl Send + 'static,
        project: impl FnOnce(Option<InspectedCaptureManifest>) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let read = CompletedCaptureRead::source_overhead(lookup, budget, operation)?;
        self.with_capture_worker(read, worker_guard, move |files, read| project(files.inspected_manifest(read)?)).await
    }
}
impl Files {
    fn inspected_manifest(&self, read: &mut CompletedCaptureRead) -> Result<Option<InspectedCaptureManifest>> {
        inspect(&self.root, &self.journal, read)
    }
}
fn inspect(
    root: &Path,
    journal: &journal::Journal,
    read: &mut CompletedCaptureRead,
) -> Result<Option<InspectedCaptureManifest>> {
    read.check()?;
    let ReadRequest::SourceOverhead { connection, observation_id } = &read.request else {
        return Err(DeliveryProblem::InvalidContract.error());
    };
    let connection = connection.clone();
    let observation_id = observation_id.clone();
    let before = journal.capture(&observation_id)?;
    if before.connection != connection {
        return Err(problem(OutboxProblem::MissingCapture));
    }
    let before_seal = manifest_seal(journal, &observation_id)?;
    let original = attempts::read_manifest_observed(root, journal, &before, read)?;
    read.check()?;
    recheck(journal, &before, &connection, before_seal.as_deref())?;
    read.check()?;
    original
        .map(|original| {
            if before_seal.as_deref().is_some_and(|sha| sha != original.sha256)
                || original.sealed != before_seal.is_some()
            {
                return Err(DeliveryProblem::Conflict.error());
            }
            Ok(InspectedCaptureManifest {
                connection,
                manifest: original.manifest,
                sha256: original.sha256,
                sealed: original.sealed,
                bytes_read: read.bytes_read,
            })
        })
        .transpose()
}
fn recheck(
    journal: &journal::Journal,
    before: &journal::Capture,
    connection: &str,
    before_seal: Option<&str>,
) -> Result<()> {
    let after = journal.capture(&before.identity.observation_id)?;
    let after_seal = manifest_seal(journal, &before.identity.observation_id)?;
    if before.identity != after.identity
        || before.request != after.request
        || before.connection != after.connection
        || after.connection != connection
        || before.state != after.state
        || before.source_work != after.source_work
        || before_seal != after_seal.as_deref()
    {
        return Err(DeliveryProblem::Conflict.error());
    }
    Ok(())
}
fn manifest_seal(journal: &journal::Journal, id: &str) -> Result<Option<String>> {
    journal
        .conn
        .query_row("SELECT manifest_sha FROM captures WHERE observation_id=?1", [id], |row| row.get(0))
        .map_err(super::storage)
}
#[cfg(test)]
mod tests;
