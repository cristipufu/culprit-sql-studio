//! One bounded temporary batch at a time. Only complete native EOF verification creates an upload capability.
use super::{
    ProjectionCatalog,
    envelope::Envelope,
    indexes::{ExpectedRow, IndexKey},
    projection,
};
use crate::delivery::{
    contract::MAX_RANGE_BYTES,
    recovery::{CommittedRange, RecoveryRecords, RecoveryStore},
};
use culprit_core::{Result, telemetry::DeliveryProblem};
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio::{
    io::AsyncWriteExt,
    sync::{OwnedSemaphorePermit, Semaphore},
};

pub const TEMP_PREFIX: &str = "culprit-analytical-projection-";

struct SpoolOwner {
    directory: PathBuf,
    _lock: crate::durable_fs::ExclusiveFile,
}
impl SpoolOwner {
    fn open(directory: &Path) -> Result<Self> {
        if !directory.try_exists().map_err(storage)? {
            crate::durable_fs::create_directory(directory).map_err(storage)?;
        }
        if !fs::symlink_metadata(directory).map_err(storage)?.file_type().is_dir() {
            return Err(invalid());
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(directory, fs::Permissions::from_mode(0o700)).map_err(storage)?;
        }
        // One owner may clean its abandoned projection files. Native recovery artifacts never enter this directory.
        let lock_path = directory.join("projection-owner.lock");
        match crate::durable_fs::create_file(&lock_path) {
            Ok(_) => (),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => (),
            Err(error) => return Err(storage(error)),
        }
        let lock = crate::durable_fs::own_file(&lock_path).map_err(|_| storage(()))?;
        for entry in fs::read_dir(directory).map_err(storage)? {
            let entry = entry.map_err(storage)?;
            if entry.file_name().to_string_lossy().starts_with(TEMP_PREFIX) {
                if !entry.file_type().map_err(storage)?.is_file() {
                    return Err(invalid());
                }
                fs::remove_file(entry.path()).map_err(storage)?;
            }
        }
        Ok(Self { directory: directory.into(), _lock: lock })
    }
}
pub struct BatchEncoder {
    catalog: Arc<dyn ProjectionCatalog>,
    recovery: Arc<dyn RecoveryStore>,
    spool: Arc<SpoolOwner>,
    batches: Arc<Semaphore>,
}
impl BatchEncoder {
    /// Use a dedicated scratch directory on a bounded volume, separate from the source outbox.
    pub fn new(catalog: Arc<dyn ProjectionCatalog>, recovery: Arc<dyn RecoveryStore>, scratch: &Path) -> Result<Self> {
        Ok(Self {
            catalog,
            recovery,
            spool: Arc::new(SpoolOwner::open(scratch)?),
            batches: Arc::new(Semaphore::new(1)),
        })
    }
    pub async fn encode(&self, range: &CommittedRange, batch_id: &str) -> Result<EncodedBatch> {
        if range.store_id() != self.recovery.id() {
            return Err(DeliveryProblem::DestinationChanged.error());
        }
        if range.manifest().contract.schema_digest() != self.catalog.digest() {
            return Err(DeliveryProblem::UnsupportedSchema.error());
        }
        let permit = self.batches.clone().acquire_owned().await.map_err(|_| storage(()))?;
        let batch = range.manifest().batches.iter().find(|batch| batch.batch_id == batch_id).ok_or_else(invalid)?;
        if !batch.delivery_required {
            return Err(DeliveryProblem::UnsupportedSchema.error());
        }
        let schema = self.catalog.schema(batch.stream)?;
        let envelope = Envelope::committed(range, batch_id)?;
        let temporary =
            tempfile::Builder::new().prefix(TEMP_PREFIX).tempfile_in(&self.spool.directory).map_err(storage)?;
        let mut file = tokio::fs::File::from_std(temporary.reopen().map_err(storage)?);
        let mut rows = RecoveryRecords::open(self.recovery.as_ref(), batch).await?;
        let mut digests = Vec::with_capacity(batch.rows.len());
        let mut bytes = 0u64;
        let mut hash = Sha256::new();
        while let Some(row) = rows.next().await? {
            let input = envelope.input(digests.len() + 1, &row)?;
            let values = schema.project(&input)?;
            let wire = schema.encode(&values)?;
            bytes = bytes
                .checked_add(wire.len() as u64)
                .filter(|bytes| *bytes <= MAX_RANGE_BYTES)
                .ok_or_else(|| DeliveryProblem::Capacity.error())?;
            file.write_all(&wire).await.map_err(storage)?;
            hash.update(&wire);
            digests.push(ExpectedRow::new(projection::digest(&values)?, wire.len())?);
        }
        if digests.len() != batch.rows.len() {
            return Err(invalid());
        }
        file.flush().await.map_err(storage)?;
        drop(file);
        let index_key = IndexKey::for_batch(range, batch_id, schema.digest())?;
        let sha256 = hash.finalize().iter().map(|byte| format!("{byte:02x}")).collect();
        Ok(EncodedBatch {
            file: temporary,
            table: schema.table().into(),
            batch_id: batch_id.into(),
            bytes,
            sha256,
            index_key,
            digests,
            _permit: permit,
            _spool: self.spool.clone(),
        })
    }
}
/// Its private constructor is reached only after source EOF, checksum, membership and projection validation.
pub struct EncodedBatch {
    file: tempfile::NamedTempFile,
    pub table: String,
    pub batch_id: String,
    pub bytes: u64,
    pub sha256: String,
    pub index_key: IndexKey,
    pub digests: Vec<ExpectedRow>,
    _permit: OwnedSemaphorePermit,
    _spool: Arc<SpoolOwner>,
}
impl EncodedBatch {
    pub fn path(&self) -> &Path {
        self.file.path()
    }
}
fn invalid() -> culprit_core::Error {
    DeliveryProblem::Integrity.error()
}
fn storage(_: impl std::fmt::Debug) -> culprit_core::Error {
    DeliveryProblem::Storage.error()
}
