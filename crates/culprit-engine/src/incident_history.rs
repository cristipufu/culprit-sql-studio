//! Source-free revision intake. Authentication belongs to the original archive/Outbox reader; JSONL is unverified.
use culprit_checks::incident_history::{HistoryRevision, explain};
use culprit_core::{IncidentHistoryProvenance, IncidentHistoryReport, Result, limits::OperationHandle, telemetry::*};
use std::io::{BufRead, BufReader, Read};
use std::path::Path;

/// Local artifact limits are independent of native collection, delivery and archive-read budgets.
pub const MAX_HISTORY_FILE_BYTES: u64 = 8 * 1024 * 1024;
pub const MAX_HISTORY_LINE_BYTES: usize = 256 * 1024;
pub const MAX_HISTORY_FILE_RECORDS: usize = 4_096;
pub const HISTORY_IMPORT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// The existing blocking-work pool retains the file and its permit after caller cancellation. No native owner exists.
pub async fn open_selected_file(path: std::path::PathBuf) -> Result<IncidentHistoryReport> {
    let guard = culprit_core::limits::OperationGuard::new(HISTORY_IMPORT_TIMEOUT)?;
    let operation = guard.handle();
    let remaining = operation.remaining();
    let language = culprit_core::i18n::current();
    tokio::time::timeout(
        remaining,
        crate::run_blocking(move || culprit_core::i18n::scope(&language.code, || import_file(&path, &operation))),
    )
    .await
    .map_err(|_| CollectionStop::TimedOut.error())?
}

/// Only the person's chosen regular file is opened. No source/credential/destination discovery occurs.
/// Use the existing bounded blocking-work owner; this handle is checked before every read and after interpretation.
pub fn import_file(path: &Path, operation: &OperationHandle) -> Result<IncidentHistoryReport> {
    check(operation)?;
    let metadata = std::fs::metadata(path).map_err(|_| unreadable())?;
    if !metadata.is_file() || metadata.len() > MAX_HISTORY_FILE_BYTES {
        return Err(DeliveryProblem::Capacity.error());
    }
    check(operation)?;
    let file = std::fs::File::open(path).map_err(|_| unreadable())?;
    let metadata = file.metadata().map_err(|_| unreadable())?;
    if !metadata.is_file() || metadata.len() > MAX_HISTORY_FILE_BYTES {
        return Err(DeliveryProblem::Capacity.error());
    }
    import_reader(file, operation)
}

/// Existing OperationalSamples ExportedRecord JSONL, not a new incident file format or source attestation.
pub fn import_reader(reader: impl Read, operation: &OperationHandle) -> Result<IncidentHistoryReport> {
    check(operation)?;
    // A bounded read includes one byte of lookahead so a growing/oversized file cannot become a complete prefix.
    let mut reader = BufReader::new(reader.take(MAX_HISTORY_FILE_BYTES + 1));
    let mut bytes = 0u64;
    let mut records = Vec::new();
    loop {
        check(operation)?;
        let mut line = Vec::new();
        let used = reader
            .by_ref()
            .take((MAX_HISTORY_LINE_BYTES + 1) as u64)
            .read_until(b'\n', &mut line)
            .map_err(|_| unreadable())?;
        if used == 0 {
            break;
        }
        check(operation)?;
        bytes = bytes.checked_add(used as u64).ok_or_else(capacity)?;
        if bytes > MAX_HISTORY_FILE_BYTES
            || line.len() > MAX_HISTORY_LINE_BYTES
            || records.len() >= MAX_HISTORY_FILE_RECORDS
        {
            return Err(capacity());
        }
        // No serde or I/O error includes a private line, path, raw value or credential in the public error.
        let row: ExportedRecord = serde_json::from_slice(&line).map_err(|_| invalid())?;
        validate_imported(&row)?;
        records.push(row);
    }
    if records.is_empty() {
        return Err(invalid());
    }
    let selected = records
        .iter()
        .map(|row| Ok(HistoryRevision { revision: revision(row)?, original: None }))
        .collect::<Result<Vec<_>>>()?;
    let report = explain(&selected, IncidentHistoryProvenance::ImportedArtifact, true)?;
    check(operation)?;
    Ok(report)
}

fn validate_imported(row: &ExportedRecord) -> Result<()> {
    if row.format_version != EXPORT_FORMAT_VERSION
        || row.representation_version != REPRESENTATION_VERSION
        || !row.capture.is_valid()
        || row.batch_id != batch_id(&row.capture.observation_id, TelemetryStream::OperationalSamples)?
        || !row.record_id.as_deref().is_some_and(valid_digest)
        || !row.record.is_valid()
    {
        return Err(invalid());
    }
    revision(row)?;
    Ok(())
}

fn revision(row: &ExportedRecord) -> Result<&DiagnosticRevision> {
    let TelemetryRecord::Operational(observation) = &row.record else { return Err(invalid()) };
    let OperationalFact::DiagnosticRevision(revision) = &observation.fact else { return Err(invalid()) };
    if observation.source != DIAGNOSTICS_SOURCE || observation.key != revision.id || !revision.is_valid() {
        return Err(invalid());
    }
    Ok(revision)
}

/// This checks canonical source/row binding; the caller must already own authenticated original bytes.
#[cfg(feature = "collector")]
pub(crate) fn from_original(
    source: &CollectionDescription,
    original: &DiagnosticCaptureRef,
    rows: &[(&ExportedRecord, u32)],
    read_complete: bool,
) -> Result<IncidentHistoryReport> {
    let SourceObservation::Diagnostics(header) = &source.observation else { return Err(invalid()) };
    if !header.is_valid()
        || !original.is_valid()
        || source.source != DIAGNOSTICS_SOURCE
        || source.captured_range()?.scope_id != original.source_scope_id
        || rows.len() > header.records as usize
        || read_complete && rows.len() != header.records as usize
    {
        return Err(invalid());
    }
    let mut positions = std::collections::BTreeSet::new();
    let mut entries = Vec::new();
    let mut references = Vec::new();
    for (row, ordinal) in rows {
        let revision = revision(row)?;
        if row.capture != original.capture
            || !positions.insert(*ordinal)
            || *ordinal >= header.records
            || !header.accepts_scope(&revision.episode.scope)
            || revision.controller_version != header.controller_version
            || ExportedRecord::new(source, &original.capture, row.record.clone())? != **row
        {
            return Err(invalid());
        }
        references.push(DiagnosticEvidenceRef::from_export(source, &original.manifest_sha256, row, *ordinal)?);
        entries.push(revision);
    }
    let selected = entries
        .iter()
        .zip(&references)
        .map(|(revision, original)| HistoryRevision { revision, original: Some(original) })
        .collect::<Vec<_>>();
    explain(&selected, IncidentHistoryProvenance::AuthenticatedCapture, read_complete)
}
fn check(operation: &OperationHandle) -> Result<()> {
    if operation.is_cancelled() {
        return Err(CollectionStop::Cancelled.error());
    }
    if operation.remaining().is_zero() {
        return Err(CollectionStop::TimedOut.error());
    }
    Ok(())
}
fn unreadable() -> culprit_core::Error {
    culprit_core::Error::invalid(culprit_core::t!("view.incident_history.import.unreadable"))
}
fn invalid() -> culprit_core::Error {
    DeliveryProblem::Integrity.error()
}
fn capacity() -> culprit_core::Error {
    DeliveryProblem::Capacity.error()
}

#[cfg(test)]
mod tests;
