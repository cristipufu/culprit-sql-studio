//! The existing manifest attests an allocated attempt independently of native success or diagnostic consumption.
use super::{Files, artifacts, journal, problem, read_identity, storage};
use culprit_core::{Result, telemetry::*};
use std::path::Path;

impl Files {
    pub(super) fn attempt_receipt(
        &self,
        identity: &CaptureIdentity,
        investigation: Option<&DiagnosticInvestigationLineage>,
    ) -> Result<DiagnosticAttemptReceipt> {
        let mut receipt = DiagnosticAttemptReceipt { capture: identity.clone(), manifest_sha256: None };
        if !receipt.is_valid() {
            return Err(integrity());
        }
        let capture = self.journal.capture(&identity.observation_id)?;
        if capture.identity != *identity {
            return Err(integrity());
        }
        if investigation.is_some_and(|owner| !owner.matches_request(&capture.request)) {
            return Err(integrity());
        }
        let Some(recorded) = read_manifest(&self.root, &self.journal, &capture)? else {
            return Ok(receipt);
        };
        if recorded.intent_key != investigation.map(|owner| owner.intent_key.clone()) {
            return Err(integrity());
        }
        receipt.manifest_sha256 = Some(recorded.sha256);
        Ok(receipt)
    }
}

/// Metadata inspection never acquires the native/capture owner lock, repairs state or opens result streams.
pub(super) fn inspect_manifest(root: &Path, observation_id: &str) -> Result<Option<CollectionManifest>> {
    let identity = read_identity(root)?;
    let journal = journal::Journal::open(&root.join("journal.sqlite3"), &identity.id, false)?;
    let capture = journal.capture(observation_id)?;
    read_manifest(root, &journal, &capture).map(|recorded| recorded.map(|recorded| recorded.manifest))
}

pub(super) struct RecordedManifest {
    pub manifest: CollectionManifest,
    pub sha256: String,
    pub intent_key: Option<String>,
    pub sealed: bool,
}

/// One bounded canonical manifest/receipt check serves both owner receipts and read-only local inspection.
pub(super) fn read_manifest(
    root: &Path,
    journal: &journal::Journal,
    capture: &journal::Capture,
) -> Result<Option<RecordedManifest>> {
    read_manifest_observed(root, journal, capture, &mut artifacts::NoObservation)
}

pub(super) fn read_manifest_observed(
    root: &Path,
    journal: &journal::Journal,
    capture: &journal::Capture,
    observer: &mut dyn artifacts::VerificationObserver,
) -> Result<Option<RecordedManifest>> {
    observer.check()?;
    capture.request.validate().map_err(|_| integrity())?;
    let sealed_sha: Option<String> = journal
        .conn
        .query_row(
            "SELECT manifest_sha FROM captures WHERE observation_id=?1",
            [&capture.identity.observation_id],
            |row| row.get(0),
        )
        .map_err(storage)?;
    if capture.state == OutboxCaptureState::Ready && sealed_sha.is_none() {
        return Err(integrity());
    }
    let directory = root.join("captures").join(&capture.identity.observation_id);
    match std::fs::symlink_metadata(&directory) {
        Ok(metadata) if !metadata.file_type().is_dir() => return Err(integrity()),
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(storage(error)),
    }
    let Some((manifest, bytes)) = artifacts::manifest_observed(&directory, observer)? else {
        if sealed_sha.is_some() {
            return Err(integrity());
        }
        validate_initial_manifest_absence(&directory)?;
        return Ok(None);
    };
    if manifest.format_version != EXPORT_FORMAT_VERSION
        || manifest.capture != capture.identity
        || manifest.request != capture.request
        || manifest.description.as_ref().is_some_and(|source| !source.matches_request(&capture.request))
    {
        return Err(integrity());
    }
    let intent_key = manifest
        .investigation_intent()
        .map_err(|_| integrity())?
        .map(|intent| intent.key())
        .transpose()
        .map_err(|_| integrity())?;
    let sha = digest(&bytes);
    if sealed_sha.as_ref().is_some_and(|sealed| sealed != &sha) {
        return Err(integrity());
    }
    Ok(Some(RecordedManifest { manifest, sha256: sha, intent_key, sealed: sealed_sha.is_some() }))
}

fn validate_initial_manifest_absence(directory: &std::path::Path) -> Result<()> {
    let entries = match std::fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(storage(error)),
    };
    // Streams are created only after the initial manifest is published. Their presence therefore proves loss
    // of an existing manifest. A failed initial publication may leave only its one unpublished temporary file.
    for (index, entry) in entries.take(2).enumerate() {
        let entry = entry.map_err(storage)?;
        if index != 0 || entry.file_name() != ".manifest.json.new" || !entry.file_type().map_err(storage)?.is_file() {
            return Err(integrity());
        }
    }
    Ok(())
}

fn integrity() -> culprit_core::Error {
    problem(OutboxProblem::Integrity)
}
