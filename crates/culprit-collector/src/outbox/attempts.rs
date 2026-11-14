//! The existing manifest attests an allocated attempt independently of native success or diagnostic consumption.
use super::{Files, artifacts, problem, storage};
use culprit_core::{Result, telemetry::*};

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
        capture.request.validate().map_err(|_| integrity())?;
        if investigation.is_some_and(|owner| !owner.matches_request(&capture.request)) {
            return Err(integrity());
        }
        let sealed_sha: Option<String> = self
            .journal
            .conn
            .query_row("SELECT manifest_sha FROM captures WHERE observation_id=?1", [&identity.observation_id], |row| {
                row.get(0)
            })
            .map_err(storage)?;
        if capture.state == OutboxCaptureState::Ready && sealed_sha.is_none() {
            return Err(integrity());
        }
        let directory = self.directory(&identity.observation_id);
        match std::fs::symlink_metadata(&directory) {
            Ok(metadata) if !metadata.file_type().is_dir() => return Err(integrity()),
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(storage(error)),
        }
        let Some((manifest, bytes)) = artifacts::manifest_if_present(&directory)? else {
            if sealed_sha.is_some() {
                return Err(integrity());
            }
            validate_initial_manifest_absence(&directory)?;
            return Ok(receipt);
        };
        if manifest.format_version != EXPORT_FORMAT_VERSION
            || manifest.capture != *identity
            || manifest.request != capture.request
            || manifest.description.as_ref().is_some_and(|source| !source.matches_request(&capture.request))
        {
            return Err(integrity());
        }
        let intent = manifest.investigation_intent().map_err(|_| integrity())?;
        if intent.as_ref().map(|intent| intent.key()).transpose().map_err(|_| integrity())?
            != investigation.map(|owner| owner.intent_key.clone())
        {
            return Err(integrity());
        }
        let sha = digest(&bytes);
        if sealed_sha.as_ref().is_some_and(|sealed| sealed != &sha) {
            return Err(integrity());
        }
        receipt.manifest_sha256 = Some(sha);
        Ok(receipt)
    }
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
