//! One bounded canonical file proof shared by capture finalization, Outbox publication and replay.
use crate::{collection_references::ObservationReferences, durable_fs};
use culprit_core::{Error, Result, telemetry::*};
use sha2::{Digest, Sha256};
use std::{
    collections::HashSet,
    fs::File,
    io::{self, BufRead, BufReader, Read, Write},
    path::Path,
};

/// The two owners preserve their established error domains; row-validation policy is shared here once.
#[derive(Clone, Copy)]
pub(crate) struct RecordFileErrors {
    pub integrity: fn() -> Error,
    pub storage: fn(io::Error) -> Error,
}

pub(crate) trait VerificationObserver {
    fn check(&self) -> Result<()> {
        Ok(())
    }
    fn caller_refused(&self, _error: &Error) -> bool {
        false
    }
    fn allowance(&self, requested: usize) -> Result<usize> {
        Ok(requested)
    }
    fn charged(&mut self, _bytes: usize) -> Result<()> {
        Ok(())
    }
    fn manifest(&mut self, _manifest: &CollectionManifest) -> Result<()> {
        Ok(())
    }
    fn row(&mut self, _row: &ExportedRecord, _ordinal: u32) -> Result<()> {
        Ok(())
    }
}

pub(crate) struct NoObservation;
impl VerificationObserver for NoObservation {}

#[derive(Default)]
pub(crate) struct RecordVerification {
    references: ObservationReferences,
    native: HashSet<NativeRecordIdentity>,
    semantic: HashSet<String>,
}
impl RecordVerification {
    pub fn references(&self) -> &ObservationReferences {
        &self.references
    }
    pub fn into_references(self) -> ObservationReferences {
        self.references
    }
    fn record(&mut self, row: &ExportedRecord, errors: RecordFileErrors) -> Result<()> {
        if !self.references.accepts_page(std::slice::from_ref(&row.record))
            || !self.native.insert(row.record.native_identity())
            || row.record_id.as_ref().is_some_and(|id| !self.semantic.insert(id.clone()))
        {
            return Err((errors.integrity)());
        }
        self.references.record(&row.record);
        Ok(())
    }
}

pub(crate) fn verify_file(
    directory: &Path,
    file: &CollectedFile,
    manifest: &CollectionManifest,
    destination: Option<&Path>,
    verification: &mut RecordVerification,
    observer: &mut dyn VerificationObserver,
    errors: RecordFileErrors,
) -> Result<()> {
    let source = manifest.description.as_ref().ok_or_else(errors.integrity)?;
    observer.check()?;
    let input = durable_fs::regular_file(&directory.join(&file.file)).map_err(|_| (errors.integrity)())?;
    if input.metadata().map_err(errors.storage)?.len() != u64::from(file.bytes) {
        return Err((errors.integrity)());
    }
    let mut input = BufReader::new(ObservedFile::new(input, u64::from(file.bytes), observer, errors));
    let mut output =
        destination.map(|path| durable_fs::create_file(&path.join(&file.file)).map_err(errors.storage)).transpose()?;
    let mut digest = Sha256::new();
    let mut count = 0;
    let mut bytes = 0;
    let mut line = Vec::new();
    loop {
        line.clear();
        let read = (&mut input)
            .take(u64::from(manifest.request.budget.page_bytes) + 1)
            .read_until(b'\n', &mut line)
            .map_err(|error| read_error(error, errors))?;
        if read == 0 {
            break;
        }
        if line.len() > manifest.request.budget.page_bytes as usize || line.last() != Some(&b'\n') {
            return Err((errors.integrity)());
        }
        bytes += read as u64;
        count += 1;
        if bytes > u64::from(file.bytes) || count > file.rows {
            return Err((errors.integrity)());
        }
        let value: serde_json::Value = serde_json::from_slice(&line).map_err(|_| (errors.integrity)())?;
        let row: ExportedRecord = serde_json::from_value(value.clone()).map_err(|_| (errors.integrity)())?;
        if serde_json::to_value(&row).map_err(|_| (errors.integrity)())? != value
            || row.capture != manifest.capture
            || row.format_version != EXPORT_FORMAT_VERSION
            || row.representation_version != REPRESENTATION_VERSION
            || row.batch_id != file.batch_id
            || row.record.stream() != file.stream
            || !row.record.is_valid()
        {
            return Err((errors.integrity)());
        }
        let expected =
            ExportedRecord::new(source, &manifest.capture, row.record.clone()).map_err(|_| (errors.integrity)())?;
        if expected != row || expected.payload_sha256()? != row.payload_sha256()? {
            return Err((errors.integrity)());
        }
        verification.record(&row, errors)?;
        input.get_mut().observer.row(&row, count - 1)?;
        digest.update(&line);
        if let Some(output) = &mut output {
            output.write_all(&line).map_err(errors.storage)?;
        }
    }
    input.get_mut().finish()?;
    let actual = digest.finalize().iter().map(|byte| format!("{byte:02x}")).collect::<String>();
    if bytes != u64::from(file.bytes) || count != file.rows || file.sha256.as_deref() != Some(&actual) {
        return Err((errors.integrity)());
    }
    if let Some(output) = output {
        output.sync_all().map_err(errors.storage)?;
    }
    Ok(())
}
pub(crate) struct ObservedFile<'a> {
    input: File,
    length: u64,
    remaining: u64,
    errors: RecordFileErrors,
    pub observer: &'a mut dyn VerificationObserver,
}
impl<'a> ObservedFile<'a> {
    pub fn new(input: File, length: u64, observer: &'a mut dyn VerificationObserver, errors: RecordFileErrors) -> Self {
        Self { input, length, remaining: length, observer, errors }
    }
    pub fn finish(&self) -> Result<()> {
        self.observer.check()?;
        if self.remaining != 0 || self.input.metadata().map_err(self.errors.storage)?.len() != self.length {
            return Err((self.errors.integrity)());
        }
        Ok(())
    }
}
impl Read for ObservedFile<'_> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        self.observer.check().map_err(io::Error::other)?;
        if self.remaining == 0 || bytes.is_empty() {
            return Ok(0);
        }
        let requested = bytes.len().min(usize::try_from(self.remaining).unwrap_or(usize::MAX));
        let allowed = self.observer.allowance(requested).map_err(io::Error::other)?.min(requested);
        if allowed == 0 {
            return Err(io::Error::other(DeliveryProblem::Capacity.error()));
        }
        let count = self.input.read(&mut bytes[..allowed])?;
        self.remaining -= count as u64;
        self.observer.charged(count).map_err(io::Error::other)?;
        Ok(count)
    }
}

pub(crate) fn read_error(error: io::Error, errors: RecordFileErrors) -> Error {
    error.get_ref().and_then(|error| error.downcast_ref::<Error>()).cloned().unwrap_or_else(|| (errors.storage)(error))
}
