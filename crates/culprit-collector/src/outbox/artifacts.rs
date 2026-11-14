//! Streaming proof of the exact files a journal commit or replay will reference.
use super::{MANIFEST_LIMIT, journal::Capture, problem, storage};
use crate::{collection_references::ObservationReferences, durable_fs};
use culprit_core::{Result, telemetry::*};
use sha2::{Digest, Sha256};
use std::{
    collections::HashSet,
    io::{BufRead, BufReader, Read, Write},
    path::Path,
};

pub(super) struct ValidatedCapture {
    pub manifest: CollectionManifest,
    pub manifest_bytes: Vec<u8>,
    pub manifest_sha: String,
    pub bytes: u64,
    pub diagnostic_checkpoint: Option<DiagnosticCheckpoint>,
    pub diagnostic_revisions: Vec<DiagnosticRevision>,
}
impl ValidatedCapture {
    pub fn range(&self) -> Result<CapturedRange> {
        let source = self.manifest.description.as_ref().ok_or_else(integrity)?;
        source.captured_range()
    }
}

pub(super) fn manifest(directory: &Path) -> Result<(CollectionManifest, Vec<u8>)> {
    let mut bytes = Vec::new();
    durable_fs::regular_file(&directory.join("manifest.json"))
        .map_err(|_| integrity())?
        .take(MANIFEST_LIMIT as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(storage)?;
    if bytes.len() > MANIFEST_LIMIT {
        return Err(integrity());
    }
    let manifest: CollectionManifest = serde_json::from_slice(&bytes).map_err(|_| integrity())?;
    Ok((manifest, bytes))
}

pub(super) fn validate(directory: &Path, capture: &Capture, destination: Option<&Path>) -> Result<ValidatedCapture> {
    if !std::fs::symlink_metadata(directory).map_err(|_| integrity())?.file_type().is_dir() {
        return Err(integrity());
    }
    let streams = capture.request.collection().streams();
    let mut names: HashSet<String> = streams.iter().map(|stream| stream.file_name().into()).collect();
    names.insert("manifest.json".into());
    for entry in std::fs::read_dir(directory).map_err(storage)? {
        let entry = entry.map_err(storage)?;
        if !entry.file_type().map_err(storage)?.is_file()
            || !entry.file_name().to_str().is_some_and(|name| names.remove(name))
        {
            return Err(integrity());
        }
    }
    if !names.is_empty() {
        return Err(integrity());
    }
    let (manifest, manifest_bytes) = manifest(directory)?;
    if manifest.format_version != EXPORT_FORMAT_VERSION {
        return Err(problem(OutboxProblem::UnsupportedFormat));
    }
    if manifest.capture != capture.identity
        || manifest.request != capture.request
        || !matches!(manifest.status, CollectionStatus::Complete)
        || !manifest.references.complete
        || manifest.files.len() != streams.len()
        || manifest.finished_at.is_none()
        || capture.source_work != SourceWorkState::Quiescent
    {
        return Err(integrity());
    }
    manifest.request.validate().map_err(|_| integrity())?;
    if manifest.read_metrics.as_ref().is_some_and(|metrics| !metrics.is_valid()) {
        return Err(integrity());
    }
    let source = manifest.description.as_ref().ok_or_else(integrity)?;
    if !source.matches_request(&manifest.request) || !source.observation.covers_request() {
        return Err(integrity());
    }
    if !source.observation.matches_file_counts(&manifest.files) {
        return Err(integrity());
    }
    if manifest.source_queries > manifest.request.budget.max_queries
        || manifest.files.iter().map(|file| u64::from(file.pages)).sum::<u64>()
            > u64::from(manifest.request.budget.max_pages)
    {
        return Err(integrity());
    }
    let mut references = ObservationReferences::default();
    let mut ids = HashSet::new();
    let mut bytes = 0;
    for (file, stream) in manifest.files.iter().zip(streams.iter().copied()) {
        if file.stream != stream
            || file.semantics != stream.semantics()
            || file.file != stream.file_name()
            || file.batch_id != batch_id(&manifest.capture.observation_id, stream)?
            || file.status != CollectionStatus::Complete
            || file.error_code.is_some()
            || u64::from(file.rows) > u64::from(file.pages) * u64::from(manifest.request.budget.page_rows)
        {
            return Err(integrity());
        }
        bytes += u64::from(file.bytes);
        if bytes > u64::from(manifest.request.budget.total_bytes) {
            return Err(integrity());
        }
        verify_file(directory, file, &manifest, destination, &mut references, &mut ids)?;
    }
    if references.coverage(true, Some(source)) != manifest.references {
        return Err(integrity());
    }
    let manifest_sha = digest(&manifest_bytes);
    bytes += manifest_bytes.len() as u64;
    Ok(ValidatedCapture {
        manifest,
        manifest_bytes,
        manifest_sha,
        bytes,
        diagnostic_checkpoint: references.diagnostic_checkpoint(),
        diagnostic_revisions: references.diagnostic_revisions,
    })
}

fn verify_file(
    directory: &Path,
    file: &CollectedFile,
    manifest: &CollectionManifest,
    destination: Option<&Path>,
    references: &mut ObservationReferences,
    ids: &mut HashSet<String>,
) -> Result<()> {
    let source = manifest.description.as_ref().ok_or_else(integrity)?;
    let input = durable_fs::regular_file(&directory.join(&file.file)).map_err(|_| integrity())?;
    if input.metadata().map_err(storage)?.len() != u64::from(file.bytes) {
        return Err(integrity());
    }
    let mut input = BufReader::new(input.take(u64::from(file.bytes) + 1));
    let mut output =
        destination.map(|path| durable_fs::create_file(&path.join(&file.file)).map_err(storage)).transpose()?;
    let mut digest = Sha256::new();
    let mut count = 0;
    let mut bytes = 0;
    let mut line = Vec::new();
    loop {
        line.clear();
        let read = (&mut input)
            .take(u64::from(manifest.request.budget.page_bytes) + 1)
            .read_until(b'\n', &mut line)
            .map_err(storage)?;
        if read == 0 {
            break;
        }
        if line.len() > manifest.request.budget.page_bytes as usize || line.last() != Some(&b'\n') {
            return Err(integrity());
        }
        bytes += read as u64;
        count += 1;
        if bytes > u64::from(file.bytes) || count > file.rows {
            return Err(integrity());
        }
        let value: serde_json::Value = serde_json::from_slice(&line).map_err(|_| integrity())?;
        let row: ExportedRecord = serde_json::from_value(value.clone()).map_err(|_| integrity())?;
        if serde_json::to_value(&row).map_err(|_| integrity())? != value
            || row.capture != manifest.capture
            || row.format_version != EXPORT_FORMAT_VERSION
            || row.representation_version != REPRESENTATION_VERSION
            || row.batch_id != file.batch_id
            || row.record.stream() != file.stream
            || !row.record.is_valid()
            || !references.accepts_page(std::slice::from_ref(&row.record))
        {
            return Err(integrity());
        }
        let expected = ExportedRecord::new(source, &manifest.capture, row.record.clone()).map_err(|_| integrity())?;
        if expected != row || !row.record_id.as_ref().is_some_and(|id| ids.insert(id.clone())) {
            return Err(integrity());
        }
        references.record(&row.record);
        digest.update(&line);
        if let Some(output) = &mut output {
            output.write_all(&line).map_err(storage)?;
        }
    }
    let actual = digest.finalize().iter().map(|byte| format!("{byte:02x}")).collect::<String>();
    if bytes != u64::from(file.bytes) || count != file.rows || file.sha256.as_deref() != Some(&actual) {
        return Err(integrity());
    }
    if let Some(output) = output {
        output.sync_all().map_err(storage)?;
    }
    Ok(())
}
fn integrity() -> culprit_core::Error {
    problem(OutboxProblem::Integrity)
}
